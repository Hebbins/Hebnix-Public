use aes::Aes256;
use aes::cipher::{BlockDecrypt, BlockEncrypt, KeyInit, generic_array::GenericArray};
use std::collections::HashSet;
use std::path::Path;

use crate::patcher::patch_core::upk;

const UPK_MAGIC: u32 = 0x9E2A83C1;
const MAX_LOGICAL_SIZE: usize = 512 * 1024 * 1024;

#[derive(Clone, Debug)]
struct Header {
    file_version: u16,
    licensee_version: u16,
    package_flags: u32,
    nonce: [u8; 12],
    last_block_size: usize,
    total_header_size: usize,
    name_count: usize,
    name_offset: usize,
    export_count: usize,
    export_offset: usize,
    import_count: usize,
    import_offset: usize,
    generation_padding_size: usize,
    chunk_table_offset: usize,
}

#[derive(Clone, Debug)]
struct Chunk {
    uncompressed_offset: usize,
    uncompressed_size: usize,
    compressed_offset: usize,
    physical_size: usize,
    block_size: usize,
    table_entry_offset: usize,
    nonce: Option<[u8; 12]>,
}

#[derive(Clone, Copy, Debug)]
pub struct FName {
    pub name_index: usize,
    pub instance: i32,
}

#[derive(Clone, Debug)]
pub struct ExportEntry {
    #[allow(dead_code)]
    pub table_index: usize,
    #[allow(dead_code)]
    pub entry_offset: usize,
    pub class_index: i32,
    pub outer_index: i32,
    pub object_name: FName,
    pub serial_size: usize,
    pub serial_offset: usize,
}

#[derive(Clone, Debug)]
struct ImportEntry {
    object_name: FName,
}

#[derive(Clone, Debug)]
pub struct Prop {
    pub name: String,
    pub tag_type: String,
    #[allow(dead_code)]
    pub struct_name: String,
    pub size: usize,
    #[allow(dead_code)]
    pub tag_offset: usize,
    pub value_offset: usize,
    pub bool_value: Option<bool>,
}

pub struct UpkPackage {
    raw: Vec<u8>,
    header: Header,
    key: [u8; 32],
    encrypted_header_size: usize,
    decrypted_header: Vec<u8>,
    chunks: Vec<Chunk>,
    pub image: Vec<u8>,
    pub names: Vec<String>,
    imports: Vec<ImportEntry>,
    pub exports: Vec<ExportEntry>,
    modified_chunks: HashSet<usize>,
    header_dirty: bool,
}

struct Cursor<'a> {
    data: &'a [u8],
    pos: usize,
}
impl<'a> Cursor<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }
    fn at(data: &'a [u8], pos: usize) -> Result<Self, String> {
        if pos > data.len() {
            return Err(format!("UPK offset {pos} is outside the file"));
        }
        Ok(Self { data, pos })
    }
    fn take(&mut self, len: usize) -> Result<&'a [u8], String> {
        let end = self.pos.checked_add(len).ok_or("UPK offset overflow")?;
        let out = self
            .data
            .get(self.pos..end)
            .ok_or("Unexpected end of UPK data")?;
        self.pos = end;
        Ok(out)
    }
    fn skip(&mut self, len: usize) -> Result<(), String> {
        self.take(len).map(|_| ())
    }
    fn u8(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16, String> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }
    fn u32(&mut self) -> Result<u32, String> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn i32(&mut self) -> Result<i32, String> {
        Ok(i32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn i64(&mut self) -> Result<i64, String> {
        Ok(i64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    fn skip_fstring(&mut self) -> Result<(), String> {
        let len = self.i32()?;
        if len < 0 {
            self.skip(
                usize::try_from(-len)
                    .map_err(|_| "Invalid FString")?
                    .checked_mul(2)
                    .ok_or("FString too large")?,
            )
        } else {
            self.skip(usize::try_from(len).map_err(|_| "Invalid FString")?)
        }
    }
    fn fstring(&mut self) -> Result<String, String> {
        let len = self.i32()?;
        if len == 0 {
            return Ok(String::new());
        }
        if len < 0 {
            let units = usize::try_from(-len).map_err(|_| "Invalid FString")?;
            let bytes = self.take(units.checked_mul(2).ok_or("FString too large")?)?;
            if bytes.len() < 2 {
                return Err("Invalid UTF-16 FString".into());
            }
            let utf16: Vec<u16> = bytes[..bytes.len() - 2]
                .chunks_exact(2)
                .map(|p| u16::from_le_bytes(p.try_into().unwrap()))
                .collect();
            return String::from_utf16(&utf16).map_err(|_| "Invalid UTF-16 FString".into());
        }
        let bytes = self.take(usize::try_from(len).map_err(|_| "Invalid FString")?)?;
        if bytes.last() != Some(&0) {
            return Err("UPK FString is not null terminated".into());
        }
        Ok(bytes[..bytes.len() - 1]
            .iter()
            .map(|b| char::from(*b))
            .collect())
    }
    fn skip_array(&mut self, size: usize) -> Result<(), String> {
        let count = non_negative(self.i32()?, "array count")?;
        self.skip(count.checked_mul(size).ok_or("Array too large")?)
    }
}

fn non_negative(value: i32, name: &str) -> Result<usize, String> {
    usize::try_from(value).map_err(|_| format!("Invalid negative {name}: {value}"))
}
fn non_negative_i64(value: i64, name: &str) -> Result<usize, String> {
    usize::try_from(value).map_err(|_| format!("Invalid negative or oversized {name}: {value}"))
}
pub fn read_i32(data: &[u8], offset: usize) -> Result<i32, String> {
    Ok(i32::from_le_bytes(
        data.get(offset..offset + 4)
            .ok_or_else(|| format!("UPK field at {offset} is out of bounds"))?
            .try_into()
            .unwrap(),
    ))
}
fn read_i64(data: &[u8], offset: usize) -> Result<i64, String> {
    Ok(i64::from_le_bytes(
        data.get(offset..offset + 8)
            .ok_or_else(|| format!("UPK field at {offset} is out of bounds"))?
            .try_into()
            .unwrap(),
    ))
}
fn write_i32(data: &mut [u8], offset: usize, value: i32) -> Result<(), String> {
    data.get_mut(offset..offset + 4)
        .ok_or_else(|| format!("UPK field at {offset} is out of bounds"))?
        .copy_from_slice(&value.to_le_bytes());
    Ok(())
}
fn write_i64(data: &mut [u8], offset: usize, value: i64) -> Result<(), String> {
    data.get_mut(offset..offset + 8)
        .ok_or_else(|| format!("UPK field at {offset} is out of bounds"))?
        .copy_from_slice(&value.to_le_bytes());
    Ok(())
}

fn read_header(raw: &[u8]) -> Result<Header, String> {
    let mut r = Cursor::new(raw);
    if r.u32()? != UPK_MAGIC {
        return Err("Invalid UPK signature".into());
    }
    let file_version = r.u16()?;
    let licensee_version = r.u16()?;
    let total_header_size = non_negative(r.i32()?, "total header size")?;
    r.skip_fstring()?;
    let package_flags = r.u32()?;
    let name_count = non_negative(r.i32()?, "name count")?;
    let name_offset = non_negative(r.i32()?, "name offset")?;
    let export_count = non_negative(r.i32()?, "export count")?;
    let export_offset = non_negative(r.i32()?, "export offset")?;
    let import_count = non_negative(r.i32()?, "import count")?;
    let import_offset = non_negative(r.i32()?, "import offset")?;
    r.i32()?;
    for _ in 0..4 {
        r.i32()?;
    }
    r.skip(16)?;
    r.skip_array(12)?;
    r.u32()?;
    r.u32()?;
    r.u32()?;
    r.skip_array(16)?;
    r.i32()?;
    let strings = non_negative(r.i32()?, "summary string count")?;
    if strings > 100_000 {
        return Err("UPK summary has too many strings".into());
    }
    for _ in 0..strings {
        r.skip_fstring()?;
    }
    let entries = non_negative(r.i32()?, "summary entry count")?;
    if entries > 100_000 {
        return Err("UPK summary has too many entries".into());
    }
    for _ in 0..entries {
        r.skip(20)?;
        r.skip_array(4)?;
    }
    let generation_padding_size = non_negative(r.i32()?, "header gap size")?;
    let chunk_table_offset = non_negative(r.i32()?, "chunk table offset")?;
    let last_block_size = non_negative(r.i32()?, "last block size")?;
    let mut nonce = [0u8; 12];
    if licensee_version >= 33 {
        nonce.copy_from_slice(r.take(12)?);
    }
    if name_count == 0
        || name_count > 1_000_000
        || export_count == 0
        || export_count > 1_000_000
        || name_offset >= raw.len()
        || total_header_size > raw.len()
    {
        return Err("Implausible UPK header".into());
    }
    Ok(Header {
        file_version,
        licensee_version,
        package_flags,
        nonce,
        last_block_size,
        total_header_size,
        name_count,
        name_offset,
        export_count,
        export_offset,
        import_count,
        import_offset,
        generation_padding_size,
        chunk_table_offset,
    })
}

fn encrypted_header_size(h: &Header) -> Result<usize, String> {
    if h.package_flags & 0x0800 != 0 {
        return h
            .total_header_size
            .checked_sub(h.last_block_size)
            .and_then(|x| x.checked_sub(h.name_offset))
            .ok_or_else(|| "Invalid encrypted header bounds".into());
    }
    let n = h
        .total_header_size
        .checked_sub(h.generation_padding_size)
        .and_then(|x| x.checked_sub(h.name_offset))
        .ok_or("Invalid encrypted header bounds")?;
    Ok(n.checked_add(15).ok_or("Encrypted header too large")? & !15)
}
fn crypt(data: &[u8], key: &[u8; 32], encrypt: bool) -> Result<Vec<u8>, String> {
    if data.len() % 16 != 0 {
        return Err("AES data is not block aligned".into());
    }
    let cipher = Aes256::new(GenericArray::from_slice(key));
    let mut output = data.to_vec();
    for block in output.chunks_exact_mut(16) {
        if encrypt {
            cipher.encrypt_block(GenericArray::from_mut_slice(block));
        } else {
            cipher.decrypt_block(GenericArray::from_mut_slice(block));
        }
    }
    Ok(output)
}
fn validate_names(data: &[u8], count: usize) -> bool {
    let mut r = Cursor::new(data);
    for _ in 0..count.min(5) {
        let start = r.pos;
        let Ok(len) = r.i32() else { return false };
        if len <= 0 || len > 512 {
            return false;
        }
        let len = len as usize;
        if r.pos
            .checked_add(len + 8)
            .is_none_or(|end| end > data.len())
            || data[r.pos + len - 1] != 0
        {
            return false;
        }
        r.pos = start + 4 + len + 8;
    }
    true
}
fn read_chunk_offset(data: &[u8], offset: usize, i64_offsets: bool) -> Result<usize, String> {
    if i64_offsets {
        non_negative_i64(read_i64(data, offset)?, "chunk offset")
    } else {
        non_negative(read_i32(data, offset)?, "chunk offset")
    }
}
pub(crate) fn crypt_ctr(data: &[u8], key: &[u8; 32], nonce: &[u8; 12]) -> Result<Vec<u8>, String> {
    let cipher = Aes256::new(GenericArray::from_slice(key));
    let mut output = data.to_vec();
    for (index, block) in output.chunks_mut(16).enumerate() {
        let counter = u32::try_from(index).map_err(|_| "AES-CTR counter overflow")?;
        let mut input = GenericArray::default();
        input[..12].copy_from_slice(nonce);
        input[12..].copy_from_slice(&counter.to_be_bytes());
        cipher.encrypt_block(&mut input);
        for (byte, mask) in block.iter_mut().zip(input.iter()) {
            *byte ^= *mask;
        }
    }
    Ok(output)
}

fn crypt_header(
    data: &[u8],
    key: &[u8; 32],
    encrypt: bool,
    header: &Header,
) -> Result<Vec<u8>, String> {
    if header.package_flags & 0x0800 != 0 {
        crypt_ctr(data, key, &header.nonce)
    } else {
        crypt(data, key, encrypt)
    }
}

fn parse_chunks(decrypted: &[u8], h: &Header, raw: &[u8]) -> Result<Vec<Chunk>, String> {
    let wide = h.licensee_version >= 22;
    let os = if wide { 8 } else { 4 };
    let normal_stride = os + 4 + os + 4;
    let count = non_negative(read_i32(decrypted, h.chunk_table_offset)?, "chunk count")?;
    if count == 0 || count >= 1000 {
        return Err(format!("Implausible compressed chunk count: {count}"));
    }
    let first = h.chunk_table_offset + 4;
    let mut stride = if h.licensee_version >= 33 {
        normal_stride + 12
    } else {
        normal_stride
    };
    if count >= 2 && h.licensee_version < 33 {
        let u0 = read_chunk_offset(decrypted, first, wide)?;
        let s0 = non_negative(read_i32(decrypted, first + os)?, "chunk size")?;
        if read_chunk_offset(decrypted, first + normal_stride + 12, wide).ok() == Some(u0 + s0) {
            stride = normal_stride + 12;
        }
    }
    if first
        .checked_add(count.checked_mul(stride).ok_or("Chunk table too large")?)
        .is_none_or(|x| x > decrypted.len())
    {
        return Err("Chunk table outside header".into());
    }
    let mut chunks = Vec::with_capacity(count);
    for index in 0..count {
        let entry = first + index * stride;
        let uo = read_chunk_offset(decrypted, entry, wide)?;
        let us = non_negative(read_i32(decrypted, entry + os)?, "chunk size")?;
        let co = read_chunk_offset(decrypted, entry + os + 4, wide)?;
        let cs = non_negative(read_i32(decrypted, entry + os + 4 + os)?, "compressed size")?;
        if us == 0
            || cs == 0
            || co.checked_add(cs).is_none_or(|e| e > raw.len())
            || (h.package_flags & 0x0800 == 0 && read_i32(raw, co).unwrap_or(0) as u32 != UPK_MAGIC)
        {
            return Err(format!("Invalid compressed chunk {index}"));
        }
        let nonce = if h.licensee_version >= 33 {
            Some(
                decrypted[entry + normal_stride..entry + normal_stride + 12]
                    .try_into()
                    .unwrap(),
            )
        } else {
            None
        };
        chunks.push(Chunk {
            uncompressed_offset: uo,
            uncompressed_size: us,
            compressed_offset: co,
            physical_size: if h.package_flags & 0x0800 != 0 { cs } else { 0 },
            block_size: 0,
            table_entry_offset: entry,
            nonce,
        });
    }
    Ok(chunks)
}

fn read_fname(r: &mut Cursor<'_>, version: u16) -> Result<FName, String> {
    let name_index = non_negative(r.i32()?, "name index")?;
    let instance = if version >= 343 {
        r.i32()?.wrapping_sub(1)
    } else {
        -1
    };
    Ok(FName {
        name_index,
        instance,
    })
}

impl UpkPackage {
    pub fn load(path: &Path) -> Result<Self, String> {
        let raw =
            std::fs::read(path).map_err(|e| format!("Could not read {}: {e}", path.display()))?;
        let header = read_header(&raw)?;
        let encrypted_header_size = encrypted_header_size(&header)?;
        let encrypted = raw
            .get(header.name_offset..header.name_offset + encrypted_header_size)
            .ok_or("File too small for encrypted header")?;
        let mut selected = None;
        for (_, key) in crate::upk_keys::embedded()? {
            let Ok(dec) = crypt_header(encrypted, &key, false, &header) else {
                continue;
            };
            if !validate_names(&dec, header.name_count) {
                continue;
            }
            if let Ok(chunks) = parse_chunks(&dec, &header, &raw) {
                selected = Some((key, dec, chunks));
                break;
            }
        }
        let (key, decrypted_header, mut chunks) =
            selected.ok_or("No embedded key can decrypt this UPK")?;
        let mut decompressed = Vec::with_capacity(chunks.len());
        let mut logical_size = raw.len();
        for (i, chunk) in chunks.iter_mut().enumerate() {
            let (data, block_size, physical_size) = if header.package_flags & 0x0800 != 0 {
                let nonce = chunk.nonce.ok_or("Encrypted chunk has no nonce")?;
                let encrypted = raw
                    .get(chunk.compressed_offset..chunk.compressed_offset + chunk.physical_size)
                    .ok_or("Encrypted chunk outside file")?;
                let decrypted = crypt_ctr(encrypted, &key, &nonce)?;
                let (data, block_size, _) = upk::decomp_chunk_at(&decrypted, 0)
                    .map_err(|e| format!("Failed to decompress UPK chunk {i}: {e:?}"))?;
                (data, block_size, chunk.physical_size)
            } else {
                let (data, block_size, end) =
                    upk::decomp_chunk_at(&raw, chunk.compressed_offset)
                        .map_err(|e| format!("Failed to decompress UPK chunk {i}: {e:?}"))?;
                (
                    data,
                    block_size,
                    end.checked_sub(chunk.compressed_offset)
                        .ok_or("Chunk boundary underflow")?,
                )
            };
            chunk.physical_size = physical_size;
            chunk.block_size = block_size as usize;
            logical_size = logical_size.max(
                chunk
                    .uncompressed_offset
                    .checked_add(data.len())
                    .ok_or("Logical size overflow")?,
            );
            decompressed.push(data);
        }
        if logical_size > MAX_LOGICAL_SIZE {
            return Err("UPK expands beyond the 512 MiB safety limit".into());
        }
        let mut image = vec![0u8; logical_size];
        image[..raw.len()].copy_from_slice(&raw);
        image[header.name_offset..header.name_offset + decrypted_header.len()]
            .copy_from_slice(&decrypted_header);
        for (chunk, data) in chunks.iter().zip(decompressed) {
            image[chunk.uncompressed_offset..chunk.uncompressed_offset + data.len()]
                .copy_from_slice(&data);
        }
        let mut r = Cursor::at(&image, header.name_offset)?;
        let mut names = Vec::with_capacity(header.name_count);
        for _ in 0..header.name_count {
            names.push(r.fstring()?);
            r.skip(8)?;
        }
        let mut r = Cursor::at(&image, header.import_offset)?;
        let mut imports = Vec::with_capacity(header.import_count);
        for _ in 0..header.import_count {
            read_fname(&mut r, header.file_version)?;
            read_fname(&mut r, header.file_version)?;
            r.i32()?;
            imports.push(ImportEntry {
                object_name: read_fname(&mut r, header.file_version)?,
            });
        }
        let mut r = Cursor::at(&image, header.export_offset)?;
        let mut exports = Vec::with_capacity(header.export_count);
        for table_index in 0..header.export_count {
            let entry_offset = r.pos;
            let class_index = r.i32()?;
            r.i32()?;
            let outer_index = r.i32()?;
            let object_name = read_fname(&mut r, header.file_version)?;
            r.i32()?;
            r.i64()?;
            let serial_size = non_negative(r.i32()?, "export size")?;
            let serial_offset = if header.licensee_version >= 22 {
                non_negative_i64(r.i64()?, "export offset")?
            } else {
                non_negative(r.i32()?, "export offset")?
            };
            r.i32()?;
            let net = non_negative(r.i32()?, "net object count")?;
            r.skip(net.checked_mul(4).ok_or("Export too large")?)?;
            r.skip(16)?;
            r.i32()?;
            exports.push(ExportEntry {
                table_index,
                entry_offset,
                class_index,
                outer_index,
                object_name,
                serial_size,
                serial_offset,
            });
        }
        Ok(Self {
            raw,
            header,
            key,
            encrypted_header_size,
            decrypted_header,
            chunks,
            image,
            names,
            imports,
            exports,
            modified_chunks: HashSet::new(),
            header_dirty: false,
        })
    }

    pub fn read_int(&self, offset: usize) -> Result<i32, String> {
        read_i32(&self.image, offset)
    }
    pub fn name_of(&self, f: FName) -> String {
        let base = self
            .names
            .get(f.name_index)
            .map(String::as_str)
            .unwrap_or("<invalid>");
        if f.instance == 0 {
            base.to_string()
        } else {
            format!("{base}_{}", f.instance - 1)
        }
    }
    pub fn class_of(&self, e: &ExportEntry) -> String {
        if e.class_index < 0 {
            self.imports
                .get((-e.class_index - 1) as usize)
                .map(|i| self.name_of(i.object_name))
                .unwrap_or_default()
        } else if e.class_index > 0 {
            self.exports
                .get((e.class_index - 1) as usize)
                .map(|x| self.name_of(x.object_name))
                .unwrap_or_default()
        } else {
            "Class".into()
        }
    }
    pub fn obj_name(&self, reference: i32) -> String {
        if reference > 0 {
            self.exports
                .get((reference - 1) as usize)
                .map(|x| self.name_of(x.object_name))
                .unwrap_or_default()
        } else if reference < 0 {
            self.imports
                .get((-reference - 1) as usize)
                .map(|i| self.name_of(i.object_name))
                .unwrap_or_default()
        } else {
            "None".into()
        }
    }
    #[allow(dead_code)]
    pub fn owner_classes(&self, e: &ExportEntry) -> Vec<String> {
        let mut result = Vec::new();
        let mut outer = e.outer_index;
        for _ in 0..64 {
            if outer <= 0 {
                break;
            }
            let Some(owner) = self.exports.get((outer - 1) as usize) else {
                break;
            };
            result.push(strip(&self.class_of(owner)).to_string());
            outer = owner.outer_index;
        }
        result
    }

    fn valid_fname(&self, f: FName) -> bool {
        f.name_index < self.names.len()
    }
    fn parse_tag(&self, raw: &[u8], offset: usize) -> (Option<Prop>, usize, bool) {
        if offset.checked_add(8).is_none_or(|x| x > raw.len()) {
            return (None, offset, false);
        }
        let parsed = (|| -> Result<(Option<Prop>, usize, bool), String> {
            let mut r = Cursor::at(raw, offset)?;
            let name_ref = read_fname(&mut r, self.header.file_version)?;
            if !self.valid_fname(name_ref) {
                return Ok((None, offset, false));
            }
            let name = self.names[name_ref.name_index].clone();
            if name == "None" {
                return Ok((None, r.pos, true));
            }
            let type_ref = read_fname(&mut r, self.header.file_version)?;
            if !self.valid_fname(type_ref) {
                return Ok((None, offset, false));
            }
            let tag_type = self.names[type_ref.name_index].clone();
            const TYPES: &[&str] = &[
                "ArrayProperty",
                "BoolProperty",
                "ByteProperty",
                "ClassProperty",
                "ComponentProperty",
                "DelegateProperty",
                "FloatProperty",
                "IntProperty",
                "InterfaceProperty",
                "MapProperty",
                "NameProperty",
                "ObjectProperty",
                "QWordProperty",
                "StrProperty",
                "StructProperty",
            ];
            if !TYPES.contains(&tag_type.as_str()) {
                return Ok((None, offset, false));
            }
            let size = non_negative(r.i32()?, "property size")?;
            if r.i32()? < 0 {
                return Ok((None, offset, false));
            }
            let mut struct_name = String::new();
            let mut bool_value = None;
            if tag_type == "StructProperty" {
                let f = read_fname(&mut r, self.header.file_version)?;
                if !self.valid_fname(f) {
                    return Ok((None, offset, false));
                }
                struct_name = self.names[f.name_index].clone();
            } else if tag_type == "ByteProperty" && self.header.file_version >= 633 {
                let f = read_fname(&mut r, self.header.file_version)?;
                if !self.valid_fname(f) {
                    return Ok((None, offset, false));
                }
            } else if tag_type == "BoolProperty" && self.header.file_version >= 673 {
                bool_value = Some(r.u8()? > 0);
            }
            let value_offset = r.pos;
            if value_offset.checked_add(size).is_none_or(|x| x > raw.len()) {
                return Ok((None, offset, false));
            }
            Ok((
                Some(Prop {
                    name,
                    tag_type,
                    struct_name,
                    size,
                    tag_offset: offset,
                    value_offset,
                    bool_value,
                }),
                value_offset + size,
                false,
            ))
        })();
        parsed.unwrap_or((None, offset, false))
    }
    /// Read the property stream immediately after UObject's net index. Unlike
    /// the recovery scanner, this never interprets texture pixels as tags.
    pub(crate) fn serialized_props(&self, e: &ExportEntry) -> Result<(Vec<Prop>, usize), String> {
        let raw = self
            .image
            .get(e.serial_offset..e.serial_offset.saturating_add(e.serial_size))
            .ok_or("Truncated export")?;
        let mut at = 4;
        let mut props = Vec::new();
        for _ in 0..4096 {
            let (prop, next, ended) = self.parse_tag(raw, at);
            if ended {
                return Ok((props, next));
            }
            props.push(prop.ok_or("Invalid serialized property stream")?);
            if next <= at {
                return Err("Property stream did not advance".into());
            }
            at = next;
        }
        Err("Too many serialized properties".into())
    }

    pub(crate) fn bulk_offset_width(&self) -> usize {
        if self.header.licensee_version >= 22 {
            8
        } else {
            4
        }
    }

    pub fn parse_props(&self, e: &ExportEntry) -> Vec<Prop> {
        let Some(raw) = self
            .image
            .get(e.serial_offset..e.serial_offset.saturating_add(e.serial_size))
        else {
            return Vec::new();
        };
        if raw.len() < 24 {
            return Vec::new();
        }
        let mut best = Vec::new();
        let mut best_score = -1i64;
        for start in 0..=raw.len() - 24 {
            let Ok(n) = read_i32(raw, start) else {
                continue;
            };
            if n < 0 || n as usize >= self.names.len() {
                continue;
            }
            let mut props = Vec::new();
            let mut at = start;
            let mut seen = HashSet::new();
            let mut ended = false;
            for _ in 0..4096 {
                if !seen.insert(at) {
                    break;
                }
                let (p, next, end) = self.parse_tag(raw, at);
                if end {
                    ended = true;
                    break;
                }
                let Some(p) = p else { break };
                props.push(p);
                at = next;
            }
            if !props.is_empty() {
                let score = props.len() as i64 * 1000
                    + if ended { 250 } else { 0 }
                    + (at.saturating_sub(start).min(512) as i64)
                    - start as i64;
                if score > best_score {
                    best_score = score;
                    best = props;
                }
            }
        }
        best
    }

    pub fn find_all_stream_name_indices(&self) -> Vec<usize> {
        let mut out = Vec::new();
        for e in &self.exports {
            if strip(&self.class_of(e)).starts_with("LevelStreaming") {
                if let Some(p) = self
                    .parse_props(e)
                    .into_iter()
                    .find(|p| p.name == "PackageName")
                {
                    if let Ok(v) = self.read_int(e.serial_offset + p.value_offset) {
                        if v >= 0 && (v as usize) < self.names.len() {
                            out.push(v as usize)
                        }
                    }
                }
            }
        }
        out
    }
    pub fn find_stream_name_index(&self) -> Option<usize> {
        let list = self.find_all_stream_name_indices();
        list.iter()
            .copied()
            .find(|i| {
                let n = self.names[*i].to_ascii_lowercase();
                ["sfx", "audio", "sound", "ambient", "tutorial"]
                    .iter()
                    .any(|x| n.contains(x))
            })
            .or_else(|| {
                list.iter().copied().find(|i| {
                    let n = self.names[*i].to_ascii_lowercase();
                    ["oob", "background", "backdrop", "sky", "bg_"]
                        .iter()
                        .any(|x| n.contains(x))
                })
            })
            .or_else(|| list.first().copied())
    }
    pub fn rename_name(&mut self, index: usize, new_name: &str) -> Result<(), String> {
        let old = self.names.get(index).ok_or("Invalid name index")?;
        if old.len() != new_name.len() || !new_name.is_ascii() {
            return Err(format!("Name length must remain {} bytes", old.len()));
        }
        let mut pos = 0usize;
        for _ in 0..index {
            let len = read_i32(&self.decrypted_header, pos)?;
            pos +=
                4 + if len >= 0 {
                    len as usize
                } else {
                    (-len) as usize * 2
                } + 8;
        }
        if read_i32(&self.decrypted_header, pos)? < 0 {
            return Err("Unicode name entries are unsupported".into());
        }
        self.decrypted_header[pos + 4..pos + 4 + new_name.len()]
            .copy_from_slice(new_name.as_bytes());
        let abs = self.header.name_offset + pos + 4;
        self.image[abs..abs + new_name.len()].copy_from_slice(new_name.as_bytes());
        self.names[index] = new_name.into();
        self.header_dirty = true;
        Ok(())
    }
    pub fn patch(&mut self, offset: usize, data: &[u8]) -> Result<(), String> {
        let end = offset.checked_add(data.len()).ok_or("Patch overflow")?;
        self.image
            .get_mut(offset..end)
            .ok_or("Patch is outside UPK")?
            .copy_from_slice(data);
        if offset >= self.header.name_offset
            && end <= self.header.name_offset + self.decrypted_header.len()
        {
            let rel = offset - self.header.name_offset;
            self.decrypted_header[rel..rel + data.len()].copy_from_slice(data);
            self.header_dirty = true;
            return Ok(());
        }
        let idx = self
            .chunks
            .iter()
            .position(|c| {
                offset >= c.uncompressed_offset
                    && end <= c.uncompressed_offset + c.uncompressed_size
            })
            .ok_or("Patch is not wholly inside one compressed chunk")?;
        self.modified_chunks.insert(idx);
        Ok(())
    }
    #[allow(dead_code)]
    pub fn patch_i32(&mut self, offset: usize, value: i32) -> Result<(), String> {
        self.patch(offset, &value.to_le_bytes())
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        std::fs::write(path, self.repacked()?)
            .map_err(|e| format!("Could not write {}: {e}", path.display()))
    }
    fn repacked(&self) -> Result<Vec<u8>, String> {
        if self.modified_chunks.is_empty() {
            let mut output = self.raw.clone();
            if self.header_dirty {
                let encrypted =
                    crypt_header(&self.decrypted_header, &self.key, true, &self.header)?;
                output
                    [self.header.name_offset..self.header.name_offset + self.encrypted_header_size]
                    .copy_from_slice(&encrypted);
            }
            return Ok(output);
        }
        let mut indexes: Vec<usize> = (0..self.chunks.len()).collect();
        indexes.sort_by_key(|i| self.chunks[*i].compressed_offset);
        let mut output = Vec::with_capacity(self.raw.len() + 4096);
        let mut old_cursor = 0;
        let mut offsets = vec![0; self.chunks.len()];
        let mut sizes = vec![0; self.chunks.len()];
        for index in indexes {
            let c = &self.chunks[index];
            if c.compressed_offset < old_cursor {
                return Err("Overlapping compressed chunks".into());
            }
            output.extend_from_slice(&self.raw[old_cursor..c.compressed_offset]);
            offsets[index] = output.len();
            if self.modified_chunks.contains(&index) {
                let span = self
                    .chunks
                    .get(index + 1)
                    .and_then(|n| n.uncompressed_offset.checked_sub(c.uncompressed_offset))
                    .unwrap_or(c.uncompressed_size)
                    .max(c.uncompressed_size);
                let payload = self
                    .image
                    .get(c.uncompressed_offset..c.uncompressed_offset + span)
                    .ok_or("Logical chunk outside image")?;
                let packed = pack_chunk(payload, c.block_size)?;
                let packed = if self.header.package_flags & 0x0800 != 0 {
                    crypt_ctr(
                        &packed,
                        &self.key,
                        &c.nonce.ok_or("Encrypted chunk has no nonce")?,
                    )?
                } else {
                    packed
                };
                sizes[index] = packed.len();
                output.extend_from_slice(&packed)
            } else {
                let end = c.compressed_offset + c.physical_size;
                output.extend_from_slice(
                    self.raw
                        .get(c.compressed_offset..end)
                        .ok_or("Compressed chunk outside file")?,
                );
                sizes[index] = c.physical_size
            }
            old_cursor = c.compressed_offset + c.physical_size;
        }
        output.extend_from_slice(self.raw.get(old_cursor..).ok_or("Invalid last chunk")?);
        let wide = self.header.licensee_version >= 22;
        let os = if wide { 8 } else { 4 };
        let mut header = self.decrypted_header.clone();
        for (i, c) in self.chunks.iter().enumerate() {
            let field = c.table_entry_offset + os + 4;
            if wide {
                write_i64(&mut header, field, offsets[i] as i64)?;
                write_i32(&mut header, field + 8, sizes[i] as i32)?
            } else {
                write_i32(&mut header, field, offsets[i] as i32)?;
                write_i32(&mut header, field + 4, sizes[i] as i32)?
            }
        }
        let encrypted = crypt_header(&header, &self.key, true, &self.header)?;
        output[self.header.name_offset..self.header.name_offset + self.encrypted_header_size]
            .copy_from_slice(&encrypted);
        Ok(output)
    }
}

fn pack_chunk(payload: &[u8], block_size: usize) -> Result<Vec<u8>, String> {
    if payload.is_empty() {
        return Err("Cannot compress empty chunk".into());
    }
    let bs = if block_size == 0 { 131072 } else { block_size };
    let mut blocks = Vec::new();
    for block in payload.chunks(bs) {
        blocks.push(upk::zlib_compress(block, 9).map_err(|e| format!("Compression failed: {e:?}"))?)
    }
    let compressed: usize = blocks.iter().map(Vec::len).sum();
    let header_len = 16 + blocks.len() * 8;
    let mut out = vec![0u8; header_len + compressed];
    upk::write_u32(&mut out, 0, UPK_MAGIC);
    upk::write_u32(&mut out, 4, bs as u32);
    upk::write_u32(&mut out, 8, compressed as u32);
    upk::write_u32(&mut out, 12, payload.len() as u32);
    let mut pos = header_len;
    for (i, b) in blocks.iter().enumerate() {
        upk::write_u32(&mut out, 16 + i * 8, b.len() as u32);
        upk::write_u32(&mut out, 20 + i * 8, bs.min(payload.len() - i * bs) as u32);
        out[pos..pos + b.len()].copy_from_slice(b);
        pos += b.len()
    }
    Ok(out)
}

pub fn strip(name: &str) -> &str {
    name.strip_suffix("_-2").unwrap_or(name)
}

#[cfg(test)]
mod local_tests {
    use super::*;

    #[test]
    #[ignore = "Requires the installed game; set HEBNIX_UPK_DIR"]
    fn opens_rlupk_key_additions() {
        let directory = std::path::PathBuf::from(
            std::env::var_os("HEBNIX_UPK_DIR").expect("Set HEBNIX_UPK_DIR"),
        );
        for name in [
            "hat_DJ_T_SF.upk",
            "hat_fnlpB_T_SF.upk",
            "skin_octane_LDJ_T_SF.upk",
            "wheel_Vindert_T_SF.upk",
        ] {
            assert!(
                !UpkPackage::load(&directory.join(name))
                    .unwrap()
                    .names
                    .is_empty(),
                "{name}"
            );
        }
    }

    #[test]
    #[ignore = "Requires the installed game; set HEBNIX_UPK_DIR"]
    fn opens_full_encrypted_dev_boost() {
        let directory = std::path::PathBuf::from(
            std::env::var_os("HEBNIX_UPK_DIR").expect("Set HEBNIX_UPK_DIR"),
        );
        for name in [
            "boost_alphadevreward_SF.upk",
            "Boost_AlphaDevReward_T_SF.upk",
        ] {
            let package = UpkPackage::load(&directory.join(name)).unwrap();
            assert!(!package.names.is_empty(), "{name}");
            assert!(package.header.package_flags & 0x0800 != 0, "{name}");
            assert_eq!(package.repacked().unwrap(), package.raw, "{name}");
        }
        let thumbnail = crate::cosmetic_thumbnail::extract_png(
            &directory.join("Boost_AlphaDevReward_T_SF.upk"),
            "boosts",
        )
        .unwrap();
        assert!(image::load_from_memory(&thumbnail).is_ok());

        let mut package =
            UpkPackage::load(&directory.join("Boost_AlphaDevReward_T_SF.upk")).unwrap();
        let chunk = &package.chunks[0];
        let offset = chunk.uncompressed_offset + chunk.uncompressed_size / 2;
        let changed = package.image[offset] ^ 1;
        package.patch(offset, &[changed]).unwrap();
        let output =
            std::env::temp_dir().join(format!("hebnix-dev-boost-{}.upk", std::process::id()));
        std::fs::write(&output, package.repacked().unwrap()).unwrap();
        let reloaded = UpkPackage::load(&output);
        std::fs::remove_file(&output).unwrap();
        assert_eq!(reloaded.unwrap().image[offset], changed);
    }
}
