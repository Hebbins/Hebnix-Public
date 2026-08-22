// crates/hebnix-app/src/decal_patcher.rs
use aes::Aes256;
use aes::cipher::{BlockDecrypt, BlockEncrypt, KeyInit, generic_array::GenericArray};
use crossbeam_channel::Sender;
use eframe::egui;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::config::Config;
use crate::messages::AppMsg;

const SKINS_CATALOG: &str = include_str!("../../assets/catalogs/skins.json");
use crate::patch_core::upk;

// UPK magic constant
const UPK_MAGIC: u32 = 0x9E2A83C1;
const MAX_LOGICAL_PACKAGE_SIZE: usize = 512 * 1024 * 1024;
const BLACK_DXT5_BLOCK: [u8; 16] = [255, 255, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 85, 85, 85, 85];

// ============================================================================
// UPK STRUCTS - Matches boost_patcher.rs
// ============================================================================

#[derive(Clone, Debug)]
struct PackageHeader {
    licensee_version: u16,
    total_header_size: usize,
    name_count: usize,
    name_offset: usize,
    export_count: usize,
    export_offset: usize,
    import_count: usize,
    import_offset: usize,
    generation_padding_size: usize,
    compressed_chunk_info_offset: usize,
}

#[derive(Clone, Debug)]
struct ChunkInfo {
    uncompressed_offset: usize,
    uncompressed_size: usize,
    compressed_offset: usize,
    physical_size: usize,
    block_size: usize,
    table_entry_offset: usize,
}

#[derive(Clone, Debug)]
struct ExportEntry {
    class_index: i32,
    object_name_index: usize,
    serial_size: usize,
    serial_offset: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TextureFormat {
    Dxt1,
    Dxt5,
    Bgra8,
}

#[derive(Clone, Debug)]
struct TextureMip {
    tfc_offset: usize,
    disk_size: usize,
    memory_size: usize,
    format: TextureFormat,
    offset_field: usize,
    memory_size_field: usize,
    legacy_offset: bool,
}

#[derive(Clone, Debug)]
struct TextureExport {
    export_name: String,
    tfc_name: Option<String>,
    mips: Vec<TextureMip>,
}

struct EncryptedPackage {
    raw: Vec<u8>,
    header: PackageHeader,
    key: [u8; 32],
    encrypted_header_size: usize,
    decrypted_header: Vec<u8>,
    chunks: Vec<ChunkInfo>,
    logical_data: Vec<u8>,
    names: Vec<String>,
    exports: Vec<ExportEntry>,
    modified_chunks: HashSet<usize>,
}

// ============================================================================
// BYTE CURSOR - Matches boost_patcher.rs
// ============================================================================

struct ByteCursor<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> ByteCursor<'a> {
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
        let bytes = self
            .data
            .get(self.pos..end)
            .ok_or_else(|| "Unexpected end of UPK data".to_string())?;
        self.pos = end;
        Ok(bytes)
    }

    fn skip(&mut self, len: usize) -> Result<(), String> {
        self.take(len).map(|_| ())
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
            self.skip(usize::try_from(-len).map_err(|_| "Invalid UPK string length")? * 2)
        } else {
            self.skip(usize::try_from(len).map_err(|_| "Invalid UPK string length")?)
        }
    }

    fn fstring(&mut self) -> Result<String, String> {
        let len = self.i32()?;
        if len == 0 {
            return Ok(String::new());
        }
        if len < 0 {
            let units = usize::try_from(-len).map_err(|_| "Invalid UPK string length")?;
            let bytes = self.take(units.checked_mul(2).ok_or("UPK string is too large")?)?;
            if bytes.len() < 2 {
                return Err("Invalid UTF-16 UPK string".to_string());
            }
            let mut utf16 = Vec::with_capacity(units.saturating_sub(1));
            for pair in bytes[..bytes.len() - 2].chunks_exact(2) {
                utf16.push(u16::from_le_bytes(pair.try_into().unwrap()));
            }
            return String::from_utf16(&utf16)
                .map_err(|_| "Invalid UTF-16 text in UPK name table".to_string());
        }

        let bytes = self.take(usize::try_from(len).map_err(|_| "Invalid UPK string length")?)?;
        if bytes.last() != Some(&0) {
            return Err("UPK string is not null terminated".to_string());
        }
        Ok(bytes[..bytes.len() - 1]
            .iter()
            .map(|byte| char::from(*byte))
            .collect())
    }

    fn skip_array(&mut self, element_size: usize) -> Result<(), String> {
        let count = non_negative(self.i32()?, "UPK array count")?;
        self.skip(
            count
                .checked_mul(element_size)
                .ok_or("UPK array is too large")?,
        )
    }
}

fn non_negative(value: i32, field: &str) -> Result<usize, String> {
    usize::try_from(value).map_err(|_| format!("Invalid negative {field}: {value}"))
}

fn non_negative_i64(value: i64, field: &str) -> Result<usize, String> {
    usize::try_from(value).map_err(|_| format!("Invalid negative or oversized {field}: {value}"))
}

fn read_i32_at(data: &[u8], offset: usize) -> Result<i32, String> {
    let bytes = data
        .get(offset..offset + 4)
        .ok_or_else(|| format!("UPK field at {offset} is out of bounds"))?;
    Ok(i32::from_le_bytes(bytes.try_into().unwrap()))
}

fn read_i64_at(data: &[u8], offset: usize) -> Result<i64, String> {
    let bytes = data
        .get(offset..offset + 8)
        .ok_or_else(|| format!("UPK field at {offset} is out of bounds"))?;
    Ok(i64::from_le_bytes(bytes.try_into().unwrap()))
}

fn write_i32_at(data: &mut [u8], offset: usize, value: i32) -> Result<(), String> {
    let target = data
        .get_mut(offset..offset + 4)
        .ok_or_else(|| format!("UPK field at {offset} is out of bounds"))?;
    target.copy_from_slice(&value.to_le_bytes());
    Ok(())
}

fn write_i64_at(data: &mut [u8], offset: usize, value: i64) -> Result<(), String> {
    let target = data
        .get_mut(offset..offset + 8)
        .ok_or_else(|| format!("UPK field at {offset} is out of bounds"))?;
    target.copy_from_slice(&value.to_le_bytes());
    Ok(())
}

// ============================================================================
// AES & KEY HELPERS - Matches boost_patcher.rs
// ============================================================================

fn aes_decrypt_ecb(data: &[u8], key: &[u8; 32]) -> Result<Vec<u8>, String> {
    if data.len() % 16 != 0 {
        return Err("Encrypted UPK header is not AES block aligned".to_string());
    }
    let cipher = Aes256::new(GenericArray::from_slice(key));
    let mut output = data.to_vec();
    for block in output.chunks_exact_mut(16) {
        cipher.decrypt_block(GenericArray::from_mut_slice(block));
    }
    Ok(output)
}

fn aes_encrypt_ecb(data: &[u8], key: &[u8; 32]) -> Result<Vec<u8>, String> {
    if data.len() % 16 != 0 {
        return Err("Decrypted UPK header is not AES block aligned".to_string());
    }
    let cipher = Aes256::new(GenericArray::from_slice(key));
    let mut output = data.to_vec();
    for block in output.chunks_exact_mut(16) {
        cipher.encrypt_block(GenericArray::from_mut_slice(block));
    }
    Ok(output)
}

fn load_upk_keys(_base_dir: &Path) -> Result<Vec<(usize, [u8; 32])>, String> {
    crate::upk_keys::embedded()
}

// ============================================================================
// READ PACKAGE HEADER - Matches boost_patcher.rs
// ============================================================================

fn read_package_header(raw: &[u8]) -> Result<PackageHeader, String> {
    let mut reader = ByteCursor::new(raw);
    if reader.u32()? != UPK_MAGIC {
        return Err("Invalid UPK signature".to_string());
    }
    let _file_version = reader.u16()?;
    let licensee_version = reader.u16()?;
    let total_header_size = non_negative(reader.i32()?, "total header size")?;
    reader.skip_fstring()?;
    let _package_flags = reader.u32()?;
    let name_count = non_negative(reader.i32()?, "name count")?;
    let name_offset = non_negative(reader.i32()?, "name offset")?;
    let export_count = non_negative(reader.i32()?, "export count")?;
    let export_offset = non_negative(reader.i32()?, "export offset")?;
    let import_count = non_negative(reader.i32()?, "import count")?;
    let import_offset = non_negative(reader.i32()?, "import offset")?;
    let _depends_offset = reader.i32()?;

    // Skip remaining FPackageFileSummary fields
    for _ in 0..4 {
        reader.i32()?;
    }
    reader.skip(16)?;
    reader.skip_array(12)?;
    reader.u32()?;
    reader.u32()?;
    reader.u32()?;
    reader.skip_array(16)?;
    reader.i32()?;

    let additional_string_count = non_negative(reader.i32()?, "summary string count")?;
    if additional_string_count > 100_000 {
        return Err("UPK summary contains too many strings".to_string());
    }
    for _ in 0..additional_string_count {
        reader.skip_fstring()?;
    }

    let additional_entry_count = non_negative(reader.i32()?, "summary entry count")?;
    if additional_entry_count > 100_000 {
        return Err("UPK summary contains too many entries".to_string());
    }
    for _ in 0..additional_entry_count {
        reader.skip(20)?;
        reader.skip_array(4)?;
    }

    let generation_padding_size = non_negative(reader.i32()?, "header gap size")?;
    let compressed_chunk_info_offset = non_negative(reader.i32()?, "compressed chunk info offset")?;
    reader.i32()?;

    if name_count == 0 || name_count > 1_000_000 {
        return Err(format!("Implausible UPK name count: {name_count}"));
    }
    if export_count == 0 || export_count > 1_000_000 {
        return Err(format!("Implausible UPK export count: {export_count}"));
    }
    if name_offset >= raw.len() || total_header_size > raw.len() {
        return Err("UPK header offsets are outside the file".to_string());
    }

    Ok(PackageHeader {
        licensee_version,
        total_header_size,
        name_count,
        name_offset,
        export_count,
        export_offset,
        import_count,
        import_offset,
        generation_padding_size,
        compressed_chunk_info_offset,
    })
}

fn encrypted_header_size(header: &PackageHeader) -> Result<usize, String> {
    let unpadded = header
        .total_header_size
        .checked_sub(header.generation_padding_size)
        .and_then(|size| size.checked_sub(header.name_offset))
        .ok_or("Invalid encrypted UPK header bounds")?;
    let padded = unpadded
        .checked_add(15)
        .ok_or("Encrypted UPK header is too large")?
        & !15;
    if padded == 0 {
        return Err("Encrypted UPK header is empty".to_string());
    }
    Ok(padded)
}

// ============================================================================
// CHUNK TABLE PARSING - Matches boost_patcher.rs
// ============================================================================

fn validate_name_prefix(decrypted: &[u8], name_count: usize) -> bool {
    let mut reader = ByteCursor::new(decrypted);
    for _ in 0..name_count.min(5) {
        let start = reader.pos;
        let Ok(len) = reader.i32() else {
            return false;
        };
        // A valid ANSI FString has a positive byte count including its NUL.
        // Rejecting absurd lengths prevents a random AES key from looking valid.
        if len <= 0 || len > 512 {
            return false;
        }
        let Ok(len) = usize::try_from(len) else {
            return false;
        };
        if reader
            .pos
            .checked_add(len + 8)
            .is_none_or(|end| end > decrypted.len())
        {
            return false;
        }
        if decrypted[reader.pos + len - 1] != 0 {
            return false;
        }
        reader.pos = start + 4 + len + 8;
    }
    true
}

fn read_chunk_offset(data: &[u8], offset: usize, use_i64: bool) -> Result<usize, String> {
    if use_i64 {
        non_negative_i64(read_i64_at(data, offset)?, "chunk offset")
    } else {
        non_negative(read_i32_at(data, offset)?, "chunk offset")
    }
}

fn parse_chunk_table(
    decrypted: &[u8],
    header: &PackageHeader,
    raw: &[u8],
) -> Result<Vec<ChunkInfo>, String> {
    let use_i64 = header.licensee_version >= 22;
    let offset_size = if use_i64 { 8 } else { 4 };
    let normal_stride = offset_size + 4 + offset_size + 4;
    let count = non_negative(
        read_i32_at(decrypted, header.compressed_chunk_info_offset)?,
        "compressed chunk count",
    )?;
    if count == 0 || count >= 1_000 {
        return Err(format!("Implausible compressed chunk count: {count}"));
    }
    let first_entry = header
        .compressed_chunk_info_offset
        .checked_add(4)
        .ok_or("Compressed chunk table offset overflow")?;
    let mut stride = normal_stride;

    // Detect padded chunk rows (12 extra bytes after each row)
    if count >= 2 {
        let first_uncompressed = read_chunk_offset(decrypted, first_entry, use_i64)?;
        let first_size = non_negative(
            read_i32_at(decrypted, first_entry + offset_size)?,
            "uncompressed chunk size",
        )?;
        let expected_second = first_uncompressed
            .checked_add(first_size)
            .ok_or("Uncompressed chunk offset overflow")?;
        let padded_stride = normal_stride + 12;
        if first_entry
            .checked_add(padded_stride + offset_size)
            .is_some_and(|end| end <= decrypted.len())
            && read_chunk_offset(decrypted, first_entry + padded_stride, use_i64).ok()
                == Some(expected_second)
        {
            stride = padded_stride;
        }
    }

    let table_end = first_entry
        .checked_add(
            count
                .checked_mul(stride)
                .ok_or("Chunk table is too large")?,
        )
        .ok_or("Chunk table is too large")?;
    if table_end > decrypted.len() {
        return Err("Compressed chunk table extends beyond the decrypted header".to_string());
    }

    let mut chunks = Vec::with_capacity(count);
    for index in 0..count {
        let entry = first_entry + index * stride;
        let uncompressed_offset = read_chunk_offset(decrypted, entry, use_i64)?;
        let uncompressed_size = non_negative(
            read_i32_at(decrypted, entry + offset_size)?,
            "uncompressed chunk size",
        )?;
        let compressed_offset = read_chunk_offset(decrypted, entry + offset_size + 4, use_i64)?;
        let compressed_size = non_negative(
            read_i32_at(decrypted, entry + offset_size + 4 + offset_size)?,
            "compressed chunk size",
        )?;
        if uncompressed_size == 0 || compressed_size == 0 {
            return Err("UPK chunk table contains an empty chunk".to_string());
        }
        if compressed_offset
            .checked_add(16)
            .is_none_or(|end| end > raw.len())
            || read_i32_at(raw, compressed_offset)
                .map(|v| v as u32)
                .unwrap_or(0)
                != UPK_MAGIC
        {
            return Err(format!(
                "UPK chunk {index} points to invalid compressed data at {compressed_offset}"
            ));
        }
        if index > 0 {
            let previous: &ChunkInfo = &chunks[index - 1];
            if uncompressed_offset < previous.uncompressed_offset + previous.uncompressed_size
                || compressed_offset < previous.compressed_offset
            {
                return Err("UPK chunk table is out of order".to_string());
            }
        }
        chunks.push(ChunkInfo {
            uncompressed_offset,
            uncompressed_size,
            compressed_offset,
            physical_size: 0,
            block_size: 0,
            table_entry_offset: entry,
        });
    }
    Ok(chunks)
}

// ============================================================================
// FIND PACKAGE KEY - Matches boost_patcher.rs
// ============================================================================

fn find_package_key(
    raw: &[u8],
    header: &PackageHeader,
    base_dir: &Path,
) -> Result<([u8; 32], Vec<u8>, Vec<ChunkInfo>), String> {
    let size = encrypted_header_size(header)?;
    let encrypted = raw
        .get(header.name_offset..header.name_offset + size)
        .ok_or("File is too small for its encrypted UPK header")?;
    let keys = load_upk_keys(base_dir)?;
    for (_, key) in &keys {
        let Ok(decrypted) = aes_decrypt_ecb(encrypted, key) else {
            continue;
        };
        // Do not accept a key solely because the chunk-count happens to be
        // plausible. Validate the decrypted name table prefix first, then the
        // complete compressed-chunk table. This is the same stricter key
        // selection approach used by boost_patcher.rs.
        if !validate_name_prefix(&decrypted, header.name_count) {
            continue;
        }
        let Ok(chunks) = parse_chunk_table(&decrypted, header, raw) else {
            continue;
        };
        return Ok((*key, decrypted, chunks));
    }
    Err(format!(
        "None of the {} valid keys can decrypt this UPK package",
        keys.len()
    ))
}

// ============================================================================
// PARSE NAMES & EXPORTS - Matches boost_patcher.rs
// ============================================================================

fn parse_names(data: &[u8], header: &PackageHeader) -> Result<Vec<String>, String> {
    let mut reader = ByteCursor::at(data, header.name_offset)?;
    let mut names = Vec::with_capacity(header.name_count);
    for _ in 0..header.name_count {
        names.push(reader.fstring()?);
        reader.skip(8)?;
    }
    Ok(names)
}

fn parse_exports(data: &[u8], header: &PackageHeader) -> Result<Vec<ExportEntry>, String> {
    let mut reader = ByteCursor::at(data, header.export_offset)?;
    let use_i64 = header.licensee_version >= 22;
    let mut exports = Vec::with_capacity(header.export_count);
    for _ in 0..header.export_count {
        let class_index = reader.i32()?;
        reader.i32()?;
        reader.i32()?;
        let object_name_index = non_negative(reader.i32()?, "export name index")?;
        reader.i32()?;
        reader.i32()?;
        reader.i64()?; // serial_size is u64 in some versions
        let serial_size = non_negative(reader.i32()?, "export serial size")?;
        let serial_offset = if use_i64 {
            non_negative_i64(reader.i64()?, "export serial offset")?
        } else {
            non_negative(reader.i32()?, "export serial offset")?
        };
        reader.i32()?;
        let net_object_count = non_negative(reader.i32()?, "export net object count")?;
        reader.skip(
            net_object_count
                .checked_mul(4)
                .ok_or("Export row is too large")?,
        )?;
        reader.skip(16)?;
        reader.i32()?;
        exports.push(ExportEntry {
            class_index,
            object_name_index,
            serial_size,
            serial_offset,
        });
    }
    Ok(exports)
}

// ============================================================================
// ENCRYPTED PACKAGE - Matches boost_patcher.rs EncryptedPackage
// ============================================================================

fn startup_upk_field_override(body_id: i32, field_key: &str) -> Option<&'static str> {
    // Direct port of the C# StartupUpkFieldOverrides table.
    // Body 23 is Octane in the current catalog and pins its diffuse export.
    if body_id == 23 {
        match field_key {
            "diffuse" | "1_diffuse_skin" => Some("Pepe_Body_D"),
            "curvaturepack" => Some("Body_Octane_Curvature_New"),
            "blankskin" => Some("Pepe_Body_BlankSkin"),
            _ => None,
        }
    } else {
        None
    }
}

fn keywords_for_field(field_key: &str) -> &'static [&'static str] {
    // Direct port of C# FieldExportKeywords + KeywordTokens.
    match field_key {
        "diffuse" | "1_diffuse_skin" => &["_d", "_diffuse", "_basecolor"],
        "curvaturepack" => &["curvature"],
        "masks" | "bodymasks" | "2_diffuse_skin_mask" => &["mask", "_rgb"],
        "normal" => &["_n", "_normal"],
        "f1detailnormal" | "f2detailnormal" => &["detailnormal", "detail_n"],
        "detail_emissive" => &["emissive", "_e"],
        "trimsheet" => &["_logo"],
        key if key.contains("diffuse") => &["_d", "_diffuse", "_basecolor"],
        key if key.contains("normal") => &["_n", "_normal"],
        key if key.contains("mask") => &["mask", "_rgb"],
        key if key.contains("emissive") => &["emissive", "_e"],
        key if key.contains("curvature") => &["curvature"],
        key if key.contains("metallic") => &["metallic", "pbr"],
        key if key.contains("roughness") => &["roughness", "pbr"],
        key if key.contains("pbr") => &["pbr"],
        _ => &[],
    }
}

fn effective_field_key_for_matching(field_key: &str, total_field_count: usize) -> String {
    // Direct port of C# EffectiveFieldKeyForMatching.
    let key = field_key.to_ascii_lowercase();
    if total_field_count != 1 || !keywords_for_field(&key).is_empty() {
        return key;
    }
    "diffuse".to_string()
}

fn is_non_diffuse_export(name: &str) -> bool {
    const HINTS: &[&str] = &[
        "mask",
        "rgb",
        "normal",
        "curvature",
        "emissive",
        "pbr",
        "metallic",
        "roughness",
        "guide",
        "autouni",
        "bevel",
        "specular",
        "gloss",
        "cavity",
        "_ao",
        "clut",
        "logo",
    ];
    HINTS.iter().any(|hint| name.contains(hint))
}

fn texture_name_matches(name: &str, keyword: &str) -> bool {
    // Direct port of the local C# Matches() function.
    if keyword.starts_with('_') && !keyword.starts_with("_rgb") {
        name.ends_with(keyword)
    } else {
        name.ends_with(keyword) || name.contains(keyword)
    }
}

fn match_texture_export<'a>(
    body_id: i32,
    field_key: &str,
    textures: &'a [TextureExport],
    exclude: Option<&str>,
    skin_specific_upk: bool,
) -> Option<&'a TextureExport> {
    let field_key = field_key.to_ascii_lowercase();

    // C# ResolveTextureForField first checks a body-specific pinned export.
    if let Some(pinned_name) = startup_upk_field_override(body_id, &field_key) {
        if let Some(texture) = textures.iter().find(|texture| {
            !exclude.is_some_and(|x| texture.export_name.eq_ignore_ascii_case(x))
                && texture.export_name.eq_ignore_ascii_case(pinned_name)
        }) {
            return Some(texture);
        }
    }

    let keywords = keywords_for_field(&field_key);
    if keywords.is_empty() {
        return None;
    }

    let is_skin_variant = field_key.contains("skin") || field_key.contains("esport");
    let mut candidates: Vec<&TextureExport> = Vec::new();

    // C# builds a de-duplicated candidate list in keyword order.
    for keyword in keywords {
        for texture in textures {
            let name = &texture.export_name;
            if exclude.is_some_and(|x| name.eq_ignore_ascii_case(x)) {
                continue;
            }
            if candidates
                .iter()
                .any(|existing| existing.export_name.eq_ignore_ascii_case(name))
            {
                continue;
            }
            let lower = name.to_ascii_lowercase();
            if texture_name_matches(&lower, keyword) {
                candidates.push(texture);
            }
        }
    }

    // C# fallback: on a skin-specific UPK, a diffuse field can use the only
    // non-technical texture export even when it has no conventional suffix.
    if candidates.is_empty() {
        if skin_specific_upk && field_key.contains("diffuse") {
            let fallback: Vec<&TextureExport> = textures
                .iter()
                .filter(|texture| {
                    !exclude.is_some_and(|x| texture.export_name.eq_ignore_ascii_case(x))
                })
                .filter(|texture| !is_non_diffuse_export(&texture.export_name.to_ascii_lowercase()))
                .collect();
            if fallback.len() == 1 {
                return fallback.first().copied();
            }
        }
        return None;
    }

    // C# prefers a skin_* export for skin-variant fields.
    if is_skin_variant {
        if field_key.contains("diffuse") {
            let skin: Vec<&TextureExport> = candidates
                .iter()
                .copied()
                .filter(|texture| {
                    let lower = texture.export_name.to_ascii_lowercase();
                    (lower.starts_with("skin_") || lower.contains("_skin_"))
                        && !is_non_diffuse_export(&lower)
                })
                .collect();
            if skin.len() == 1 {
                return skin.first().copied();
            }
        }

        if let Some(skin) = candidates.iter().copied().find(|texture| {
            let lower = texture.export_name.to_ascii_lowercase();
            lower.starts_with("skin_") || lower.contains("_skin_")
        }) {
            return Some(skin);
        }
    }

    // C# prefers the sole *_basecolor texture for diffuse fields in a
    // skin-specific UPK.
    if skin_specific_upk && field_key.contains("diffuse") {
        let basecolors: Vec<&TextureExport> = candidates
            .iter()
            .copied()
            .filter(|texture| {
                texture
                    .export_name
                    .to_ascii_lowercase()
                    .ends_with("_basecolor")
            })
            .collect();
        if basecolors.len() == 1 {
            return basecolors.first().copied();
        }
    }

    // Finally, prefer the first non-skin export, otherwise the first candidate.
    candidates
        .iter()
        .copied()
        .find(|texture| {
            !texture
                .export_name
                .to_ascii_lowercase()
                .starts_with("skin_")
        })
        .or_else(|| candidates.first().copied())
}

#[cfg(test)]
mod texture_matching_tests {
    use super::{TextureExport, match_texture_export};

    #[test]
    fn skin_specific_flames_rgb_mask_is_not_a_colour_diffuse() {
        let textures = vec![TextureExport {
            export_name: "Pepe_Body_Flames_RGB".to_string(),
            tfc_name: Some("Textures4".to_string()),
            mips: Vec::new(),
        }];
        let matched = match_texture_export(23, "1_diffuse_skin", &textures, None, true);
        assert!(matched.is_none());
    }

    #[test]
    fn octane_body_bevel_mask_does_not_match_diffuse_but_startup_pin_does() {
        let body = vec![
            TextureExport {
                export_name: "Body_Octane_Bevel_N".to_string(),
                tfc_name: Some("Textures4".to_string()),
                mips: Vec::new(),
            },
            TextureExport {
                export_name: "Body_Octane_Bevel_RGB".to_string(),
                tfc_name: Some("Textures4".to_string()),
                mips: Vec::new(),
            },
        ];
        assert!(match_texture_export(23, "1_diffuse_skin", &body, None, false).is_none());

        let startup = vec![TextureExport {
            export_name: "Pepe_Body_D".to_string(),
            tfc_name: Some("Textures4".to_string()),
            mips: Vec::new(),
        }];
        assert_eq!(
            match_texture_export(23, "1_diffuse_skin", &startup, None, false)
                .map(|texture| texture.export_name.as_str()),
            Some("Pepe_Body_D")
        );
    }
}

fn is_tfc_name(name: &str) -> bool {
    name.strip_prefix("Textures")
        .is_some_and(|suffix| suffix.chars().all(|c| c.is_ascii_digit()))
}

fn read_texture_format(
    data: &[u8],
    names: &[String],
    serial_offset: usize,
    serial_size: usize,
) -> Option<TextureFormat> {
    let format_index = names.iter().position(|name| name == "Format")?;
    let byte_property_index = names.iter().position(|name| name == "ByteProperty")?;
    let end = serial_offset.checked_add(serial_size)?.min(data.len());
    for pos in (serial_offset..end.saturating_sub(40)).step_by(4) {
        if read_i32_at(data, pos).ok()? != format_index as i32
            || read_i32_at(data, pos + 4).ok()? != 0
            || read_i32_at(data, pos + 8).ok()? != byte_property_index as i32
            || read_i32_at(data, pos + 12).ok()? != 0
            || read_i32_at(data, pos + 16).ok()? != 8
            || read_i32_at(data, pos + 20).ok()? != 0
        {
            continue;
        }
        let value_index = usize::try_from(read_i32_at(data, pos + 32).ok()?).ok()?;
        return match names.get(value_index).map(String::as_str) {
            Some("PF_DXT1") => Some(TextureFormat::Dxt1),
            Some("PF_DXT5") => Some(TextureFormat::Dxt5),
            Some("PF_A8R8G8B8") => Some(TextureFormat::Bgra8),
            _ => None,
        };
    }
    None
}

fn sanitize_mips(mips: Vec<TextureMip>) -> Vec<TextureMip> {
    let mut result: Vec<TextureMip> = Vec::new();
    for mip in mips {
        if let Some(previous) = result.last() {
            if mip.format != result[0].format
                || mip.memory_size >= previous.memory_size
                || mip.disk_size >= previous.disk_size
            {
                break;
            }
        }
        result.push(mip);
    }
    result
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn scan_legacy_tfc_mips(
    data: &[u8],
    serial_offset: usize,
    serial_size: usize,
    tfc_path: &Path,
    format_hint: Option<TextureFormat>,
) -> Vec<TextureMip> {
    // Match C# ScanLegacyTfcMips: do not load the whole multi-gigabyte TFC
    // into memory for every Texture2D export. Seek to candidate offsets and
    // read only the 16-byte TFC chunk header.
    let Ok(mut tfc) = fs::File::open(tfc_path) else {
        return Vec::new();
    };
    let Ok(tfc_len) = tfc.metadata().map(|metadata| metadata.len()) else {
        return Vec::new();
    };
    let Some(serial) =
        data.get(serial_offset..serial_offset.saturating_add(serial_size).min(data.len()))
    else {
        return Vec::new();
    };
    let mut offsets = HashSet::new();
    let mut mips = Vec::new();
    for relative in 0..serial.len().saturating_sub(8) {
        let Ok(memory_size) = usize::try_from(i32::from_le_bytes(
            serial[relative..relative + 4].try_into().unwrap(),
        )) else {
            continue;
        };
        let tfc_offset =
            u32::from_le_bytes(serial[relative + 4..relative + 8].try_into().unwrap()) as usize;
        if memory_size == 0
            || memory_size > 50_000_000
            || tfc_offset <= 65_536
            || (tfc_offset as u64)
                .checked_add(16)
                .is_none_or(|end| end > tfc_len)
            || !offsets.insert(tfc_offset)
        {
            continue;
        }
        if tfc.seek(SeekFrom::Start(tfc_offset as u64)).is_err() {
            continue;
        }
        let mut header = [0u8; 16];
        if tfc.read_exact(&mut header).is_err() {
            continue;
        }
        if u32::from_le_bytes(header[0..4].try_into().unwrap()) != UPK_MAGIC {
            continue;
        }
        let disk_size = u32::from_le_bytes(header[12..16].try_into().unwrap()) as usize;
        let inferred = if disk_size % 4 == 0 {
            let side = ((disk_size / 4) as f64).sqrt() as usize;
            (side * side * 4 == disk_size).then_some(TextureFormat::Bgra8)
        } else {
            None
        }
        .or_else(|| {
            let side = (disk_size as f64).sqrt() as usize;
            (side * side == disk_size).then_some(TextureFormat::Dxt5)
        })
        .or_else(|| {
            let side = ((disk_size * 2) as f64).sqrt() as usize;
            (side * side == disk_size * 2).then_some(TextureFormat::Dxt1)
        });
        if let Some(format) = format_hint.or(inferred) {
            mips.push(TextureMip {
                tfc_offset,
                disk_size,
                memory_size,
                format,
                offset_field: serial_offset + relative + 4,
                memory_size_field: serial_offset + relative,
                legacy_offset: true,
            });
        }
    }
    sanitize_mips(mips)
}

fn parse_texture_exports(
    package: &EncryptedPackage,
    game_dir: &Path,
) -> Result<Vec<TextureExport>, String> {
    let data = &package.logical_data;
    let header = &package.header;
    let mut texture_class_index = None;
    for import_index in 0..header.import_count {
        let row = header
            .import_offset
            .checked_add(
                import_index
                    .checked_mul(28)
                    .ok_or("Import table is too large")?,
            )
            .ok_or("Import table offset overflow")?;
        let object_name_index = non_negative(read_i32_at(data, row + 20)?, "import name index")?;
        if package.names.get(object_name_index).map(String::as_str) == Some("Texture2D") {
            texture_class_index = Some(-((import_index as i32) + 1));
            break;
        }
    }
    let texture_class_index =
        texture_class_index.ok_or("Texture2D class not found in UPK import table")?;
    let tfc_names: Vec<(usize, &String)> = package
        .names
        .iter()
        .enumerate()
        .filter(|(_, name)| is_tfc_name(name))
        .collect();
    let mut textures = Vec::new();
    for export in package
        .exports
        .iter()
        .filter(|export| export.class_index == texture_class_index)
    {
        let Some(export_name) = package.names.get(export.object_name_index) else {
            continue;
        };
        if export.serial_offset == 0
            || export.serial_size == 0
            || export.serial_offset >= data.len()
        {
            continue;
        }
        let serial_end = export
            .serial_offset
            .saturating_add(export.serial_size)
            .min(data.len());
        let serial = &data[export.serial_offset..serial_end];
        let mut tfc_match: Option<(usize, String)> = None;
        for (name_index, name) in &tfc_names {
            let mut needle = Vec::with_capacity(8);
            needle.extend_from_slice(&(*name_index as i32).to_le_bytes());
            needle.extend_from_slice(&0i32.to_le_bytes());
            if let Some(relative) = find_bytes(serial, &needle) {
                if tfc_match.as_ref().is_none_or(|(best, _)| relative < *best) {
                    tfc_match = Some((relative, (*name).clone()));
                }
            }
        }
        let tfc_name = tfc_match.map(|(_, name)| name);
        let format_hint = read_texture_format(
            data,
            &package.names,
            export.serial_offset,
            export.serial_size,
        );
        let mut seen_offsets = HashSet::new();
        let mut mips = Vec::new();
        for relative in (16..serial.len().saturating_sub(8)).step_by(4) {
            let tfc_offset_i64 =
                i64::from_le_bytes(serial[relative..relative + 8].try_into().unwrap());
            let Ok(tfc_offset) = usize::try_from(tfc_offset_i64) else {
                continue;
            };
            if tfc_offset <= 4096
                || tfc_offset >= u32::MAX as usize
                || !seen_offsets.insert(tfc_offset)
            {
                continue;
            }
            let Ok(disk_size) = usize::try_from(i32::from_le_bytes(
                serial[relative - 8..relative - 4].try_into().unwrap(),
            )) else {
                continue;
            };
            let Ok(memory_size) = usize::try_from(i32::from_le_bytes(
                serial[relative - 4..relative].try_into().unwrap(),
            )) else {
                continue;
            };
            if disk_size == 0 || memory_size == 0 || memory_size >= 50_000_000 {
                continue;
            }
            let format = match format_hint {
                Some(TextureFormat::Bgra8)
                    if {
                        let side = ((disk_size / 4) as f64).sqrt() as usize;
                        side * side * 4 == disk_size
                    } =>
                {
                    TextureFormat::Bgra8
                }
                Some(TextureFormat::Dxt1) => TextureFormat::Dxt1,
                _ if {
                    let side = (disk_size as f64).sqrt() as usize;
                    side * side == disk_size
                } =>
                {
                    TextureFormat::Dxt5
                }
                _ if {
                    let side = ((disk_size * 2) as f64).sqrt() as usize;
                    side * side == disk_size * 2
                } =>
                {
                    TextureFormat::Dxt1
                }
                _ => continue,
            };
            let block_count = disk_size.div_ceil(131_072);
            if memory_size < 16 + block_count * 8 {
                continue;
            }
            mips.push(TextureMip {
                tfc_offset,
                disk_size,
                memory_size,
                format,
                offset_field: export.serial_offset + relative,
                memory_size_field: export.serial_offset + relative - 4,
                legacy_offset: false,
            });
        }
        let mut mips = sanitize_mips(mips);
        if mips.is_empty() {
            if let Some(name) = &tfc_name {
                mips = scan_legacy_tfc_mips(
                    data,
                    export.serial_offset,
                    export.serial_size,
                    &game_dir.join(format!("{name}.tfc")),
                    format_hint,
                );
            }
        }
        if !mips.is_empty() {
            textures.push(TextureExport {
                export_name: export_name.clone(),
                tfc_name,
                mips,
            });
        }
    }
    Ok(textures)
}

impl EncryptedPackage {
    fn load(raw: Vec<u8>, base_dir: &Path) -> Result<Self, String> {
        let header = read_package_header(&raw)?;
        let encrypted_header_size = encrypted_header_size(&header)?;
        let (key, decrypted_header, mut chunks) = find_package_key(&raw, &header, base_dir)?;

        let mut decompressed_chunks = Vec::with_capacity(chunks.len());
        let mut logical_size = raw.len();
        for (index, chunk) in chunks.iter_mut().enumerate() {
            let (decompressed, block_size, physical_end) =
                upk::decomp_chunk_at(&raw, chunk.compressed_offset).map_err(|error| {
                    format!("Failed to decompress UPK chunk {index}: {error:?}")
                })?;
            chunk.physical_size = physical_end
                .checked_sub(chunk.compressed_offset)
                .ok_or("Compressed chunk boundary underflow")?;
            chunk.block_size = usize::try_from(block_size)
                .map_err(|_| "Compressed chunk block size is too large")?;
            logical_size = logical_size.max(
                chunk
                    .uncompressed_offset
                    .checked_add(decompressed.len())
                    .ok_or("Logical UPK size overflow")?,
            );
            decompressed_chunks.push(decompressed);
        }
        if logical_size > MAX_LOGICAL_PACKAGE_SIZE {
            return Err(format!(
                "UPK expands to {logical_size} bytes, above the safety limit"
            ));
        }

        let mut logical_data = vec![0u8; logical_size];
        logical_data[..raw.len()].copy_from_slice(&raw);
        logical_data[header.name_offset..header.name_offset + decrypted_header.len()]
            .copy_from_slice(&decrypted_header);
        for (chunk, decompressed) in chunks.iter().zip(decompressed_chunks) {
            let end = chunk.uncompressed_offset + decompressed.len();
            logical_data[chunk.uncompressed_offset..end].copy_from_slice(&decompressed);
        }

        let names = parse_names(&logical_data, &header)?;
        let exports = parse_exports(&logical_data, &header)?;
        if exports
            .iter()
            .any(|export| export.object_name_index >= names.len())
        {
            return Err("Export table contains an invalid name index".to_string());
        }

        Ok(Self {
            raw,
            header,
            key,
            encrypted_header_size,
            decrypted_header,
            chunks,
            logical_data,
            names,
            exports,
            modified_chunks: HashSet::new(),
        })
    }

    fn mark_modified_range(&mut self, start: usize, end: usize) -> Result<(), String> {
        let (index, _) = self
            .chunks
            .iter()
            .enumerate()
            .find(|(_, chunk)| {
                start >= chunk.uncompressed_offset
                    && end <= chunk.uncompressed_offset + chunk.uncompressed_size
            })
            .ok_or("Texture data is not contained in a compressed UPK chunk")?;
        self.modified_chunks.insert(index);
        Ok(())
    }
}

// ============================================================================
// COMPRESS & REPACK - Matches boost_patcher.rs
// ============================================================================

fn compress_into_chunk(payload: &[u8], block_size: usize) -> Result<Vec<u8>, String> {
    if payload.is_empty() {
        return Err("Cannot compress an empty UPK chunk".to_string());
    }

    let block_size = if block_size == 0 { 131_072 } else { block_size };

    let mut blocks = Vec::new();
    for block in payload.chunks(block_size) {
        blocks.push(
            upk::zlib_compress(block, 9)
                .map_err(|e| format!("Failed to compress UPK chunk: {e:?}"))?,
        );
    }

    let compressed_total: usize = blocks.iter().map(Vec::len).sum();
    let header_len = 16usize
        .checked_add(
            blocks
                .len()
                .checked_mul(8)
                .ok_or("Chunk header is too large")?,
        )
        .ok_or("Chunk header is too large")?;
    let output_len = header_len
        .checked_add(compressed_total)
        .ok_or("Compressed chunk is too large")?;
    let mut output = vec![0u8; output_len];

    upk::write_u32(&mut output, 0, UPK_MAGIC);
    upk::write_u32(
        &mut output,
        4,
        u32::try_from(block_size).map_err(|_| "UPK chunk block size is too large")?,
    );
    upk::write_u32(
        &mut output,
        8,
        u32::try_from(compressed_total).map_err(|_| "Compressed chunk is too large")?,
    );
    upk::write_u32(
        &mut output,
        12,
        u32::try_from(payload.len()).map_err(|_| "Uncompressed chunk is too large")?,
    );

    let mut output_pos = header_len;
    for (index, block) in blocks.iter().enumerate() {
        let uncompressed_start = index * block_size;
        let uncompressed_len = block_size.min(payload.len() - uncompressed_start);
        upk::write_u32(
            &mut output,
            16 + index * 8,
            u32::try_from(block.len()).map_err(|_| "Compressed block is too large")?,
        );
        upk::write_u32(
            &mut output,
            20 + index * 8,
            u32::try_from(uncompressed_len).map_err(|_| "Uncompressed block is too large")?,
        );
        output[output_pos..output_pos + block.len()].copy_from_slice(block);
        output_pos += block.len();
    }

    Ok(output)
}

fn repack_package(package: &EncryptedPackage) -> Result<Vec<u8>, String> {
    if package.modified_chunks.is_empty() {
        return Err("No UPK chunks were modified".to_string());
    }

    // Sort chunks by compressed offset
    let mut sorted_indexes: Vec<usize> = (0..package.chunks.len()).collect();
    sorted_indexes.sort_by_key(|index| package.chunks[*index].compressed_offset);

    let mut output = Vec::with_capacity(package.raw.len() + 4096);
    let mut old_cursor = 0usize;
    let mut new_offsets = vec![0usize; package.chunks.len()];
    let mut new_sizes = vec![0usize; package.chunks.len()];

    for index in sorted_indexes {
        let chunk = &package.chunks[index];
        if chunk.compressed_offset < old_cursor {
            return Err("Overlapping compressed UPK chunks; refusing to repack".to_string());
        }
        output.extend_from_slice(&package.raw[old_cursor..chunk.compressed_offset]);
        new_offsets[index] = output.len();

        if package.modified_chunks.contains(&index) {
            let logical_span = package
                .chunks
                .get(index + 1)
                .and_then(|next| {
                    next.uncompressed_offset
                        .checked_sub(chunk.uncompressed_offset)
                })
                .unwrap_or(chunk.uncompressed_size)
                .max(chunk.uncompressed_size);
            let logical_end = chunk
                .uncompressed_offset
                .checked_add(logical_span)
                .ok_or("Logical UPK chunk size overflow")?;
            let payload = package
                .logical_data
                .get(chunk.uncompressed_offset..logical_end)
                .ok_or("Logical UPK chunk extends beyond package data")?;
            let compressed = compress_into_chunk(payload, chunk.block_size)?;
            new_sizes[index] = compressed.len();
            output.extend_from_slice(&compressed);
        } else {
            let old_end = chunk
                .compressed_offset
                .checked_add(chunk.physical_size)
                .ok_or("Compressed UPK chunk size overflow")?;
            output.extend_from_slice(
                package
                    .raw
                    .get(chunk.compressed_offset..old_end)
                    .ok_or("Compressed UPK chunk extends beyond the source file")?,
            );
            new_sizes[index] = chunk.physical_size;
        }
        old_cursor = chunk.compressed_offset + chunk.physical_size;
    }
    output.extend_from_slice(
        package
            .raw
            .get(old_cursor..)
            .ok_or("Invalid final compressed chunk boundary")?,
    );

    // Update chunk table in decrypted header
    let use_i64 = package.header.licensee_version >= 22;
    let offset_size = if use_i64 { 8 } else { 4 };
    let mut decrypted_header = package.decrypted_header.clone();

    for (index, chunk) in package.chunks.iter().enumerate() {
        let compressed_offset_field = chunk.table_entry_offset + offset_size + 4;
        if use_i64 {
            write_i64_at(
                &mut decrypted_header,
                compressed_offset_field,
                i64::try_from(new_offsets[index]).map_err(|_| "Repacked UPK is too large")?,
            )?;
            write_i32_at(
                &mut decrypted_header,
                compressed_offset_field + 8,
                i32::try_from(new_sizes[index]).map_err(|_| "Repacked UPK is too large")?,
            )?;
        } else {
            write_i32_at(
                &mut decrypted_header,
                compressed_offset_field,
                i32::try_from(new_offsets[index]).map_err(|_| "Repacked UPK is too large")?,
            )?;
            write_i32_at(
                &mut decrypted_header,
                compressed_offset_field + 4,
                i32::try_from(new_sizes[index]).map_err(|_| "Repacked UPK is too large")?,
            )?;
        }
    }

    // Re-encrypt and write header
    let encrypted_header = aes_encrypt_ecb(&decrypted_header, &package.key)?;
    if encrypted_header.len() != package.encrypted_header_size {
        return Err("Re-encrypted UPK header size changed; refusing to write".to_string());
    }
    let encrypted_end = package.header.name_offset + package.encrypted_header_size;
    if encrypted_end > output.len() {
        return Err("Re-encrypted UPK header no longer fits the package".to_string());
    }
    output[package.header.name_offset..encrypted_end].copy_from_slice(&encrypted_header);

    Ok(output)
}

// ============================================================================
// PNG TO DXT - Matches boost_patcher.rs image_to_dxt5_with_alpha
// ============================================================================

fn image_to_dxt5_with_alpha(
    img: &image::RgbaImage,
    width: usize,
    height: usize,
    force_opaque_alpha: bool,
) -> Vec<u8> {
    use crate::patch_core::dxt;

    // The game uses the same BCn layout as the other patchers.  Keep the
    // colour encoder shared with patch_core (in particular its endpoint
    // ordering and flat-colour handling); a subtly malformed DXT block is
    // rendered by UE as a solid/white texture.  We only need the local path
    // below when preserving source alpha for mask textures.
    if force_opaque_alpha {
        return dxt::image_to_dxt5(img, width, height);
    }

    let mut dxt5 = vec![0u8; width.max(4) / 4 * height.max(4) / 4 * 16];
    let mut out_idx = 0;

    let mut r = [0i32; 16];
    let mut g = [0i32; 16];
    let mut b = [0i32; 16];
    let mut a = [0u8; 16];

    for y in (0..height).step_by(4) {
        for x in (0..width).step_by(4) {
            for cy in 0..4 {
                for cx in 0..4 {
                    let px = (x + cx).min(width - 1) as u32;
                    let py = (y + cy).min(height - 1) as u32;
                    let pixel = img.get_pixel(px, py).0;
                    let idx = cy * 4 + cx;
                    r[idx] = pixel[0] as i32;
                    g[idx] = pixel[1] as i32;
                    b[idx] = pixel[2] as i32;
                    a[idx] = if force_opaque_alpha { 255 } else { pixel[3] };
                }
            }

            // Alpha block - DXT5
            let mut a_max = a[0];
            let mut a_min = a[0];
            for i in 1..16 {
                if a[i] > a_max {
                    a_max = a[i];
                }
                if a[i] < a_min {
                    a_min = a[i];
                }
            }

            dxt5[out_idx] = a_max;
            dxt5[out_idx + 1] = a_min;

            let mut indices = 0u64;
            if a_max != a_min {
                let a_max = a_max as i32;
                let a_min = a_min as i32;
                let palettes = [
                    a_max,
                    a_min,
                    (6 * a_max + a_min + 3) / 7,
                    (5 * a_max + 2 * a_min + 3) / 7,
                    (4 * a_max + 3 * a_min + 3) / 7,
                    (3 * a_max + 4 * a_min + 3) / 7,
                    (2 * a_max + 5 * a_min + 3) / 7,
                    (a_max + 6 * a_min + 3) / 7,
                ];
                for i in 0..16 {
                    let mut best_dist = i32::MAX;
                    let mut best_idx = 0;
                    for j in 0..8 {
                        let dist = (a[i] as i32 - palettes[j]).abs();
                        if dist < best_dist {
                            best_dist = dist;
                            best_idx = j;
                        }
                    }
                    indices |= (best_idx as u64) << (i * 3);
                }
            }
            for i in 0..6 {
                dxt5[out_idx + 2 + i] = ((indices >> (i * 8)) & 0xFF) as u8;
            }

            // Color block - use the canonical encoder shared by the other
            // patchers.  This avoids endpoint ordering differences that can
            // make UE sample a white/invalid block.
            let color_block = dxt::dxt1_block(&r, &g, &b);
            dxt5[out_idx + 8..out_idx + 16].copy_from_slice(&color_block);

            out_idx += 16;
        }
    }
    dxt5
}

fn image_to_dxt1(img: &image::RgbaImage, width: usize, height: usize) -> Vec<u8> {
    crate::patch_core::dxt::image_to_dxt1(img, width, height)
}

fn image_to_bgra8(img: &image::RgbaImage) -> Vec<u8> {
    let mut output = Vec::with_capacity(img.len());
    for pixel in img.pixels() {
        output.extend_from_slice(&[pixel[2], pixel[1], pixel[0], pixel[3]]);
    }
    output
}

fn texture_mip_side(mip: &TextureMip) -> usize {
    match mip.format {
        TextureFormat::Dxt1 => ((mip.disk_size * 2) as f64).sqrt() as usize,
        TextureFormat::Dxt5 => (mip.disk_size as f64).sqrt() as usize,
        TextureFormat::Bgra8 => ((mip.disk_size / 4) as f64).sqrt() as usize,
    }
}

/// Match the C# patcher's per-channel resize. Resizing RGBA as a single image
/// can premultiply RGB by alpha in some image paths and damages decal colours.
fn resize_rgba_isolated(image: &image::RgbaImage, width: u32, height: u32) -> image::RgbaImage {
    use image::imageops::FilterType;
    let (source_width, source_height) = image.dimensions();
    let mut channels = std::array::from_fn(|_| image::GrayImage::new(source_width, source_height));
    for (x, y, pixel) in image.enumerate_pixels() {
        for channel in 0..4 {
            channels[channel].put_pixel(x, y, image::Luma([pixel[channel]]));
        }
    }
    let [red, green, blue, alpha] = channels
        .map(|channel| image::imageops::resize(&channel, width, height, FilterType::CatmullRom));
    image::RgbaImage::from_fn(width, height, |x, y| {
        image::Rgba([
            red.get_pixel(x, y)[0],
            green.get_pixel(x, y)[0],
            blue.get_pixel(x, y)[0],
            alpha.get_pixel(x, y)[0],
        ])
    })
}

fn build_tfc_chunk(payload: &[u8], available_compressed: Option<usize>) -> Result<Vec<u8>, String> {
    let mut blocks = Vec::new();
    for block in payload.chunks(131_072) {
        blocks.push(
            upk::zlib_compress(block, 9)
                .map_err(|error| format!("Failed to compress TFC texture block: {error:?}"))?,
        );
    }
    let compressed_size: usize = blocks.iter().map(Vec::len).sum();
    if available_compressed.is_some_and(|available| compressed_size > available) {
        return Err(format!("compressed texture needs {compressed_size} bytes"));
    }
    let stored_compressed_size = available_compressed.unwrap_or(compressed_size);
    let mut output = vec![0u8; 16 + blocks.len() * 8 + stored_compressed_size];
    output[0..4].copy_from_slice(&UPK_MAGIC.to_le_bytes());
    output[4..8].copy_from_slice(&131_072u32.to_le_bytes());
    output[8..12].copy_from_slice(&(compressed_size as u32).to_le_bytes());
    output[12..16].copy_from_slice(&(payload.len() as u32).to_le_bytes());
    let mut write_pos = 16 + blocks.len() * 8;
    for (index, block) in blocks.iter().enumerate() {
        let uncompressed_size = 131_072.min(payload.len() - index * 131_072);
        output[16 + index * 8..20 + index * 8].copy_from_slice(&(block.len() as u32).to_le_bytes());
        output[20 + index * 8..24 + index * 8]
            .copy_from_slice(&(uncompressed_size as u32).to_le_bytes());
        output[write_pos..write_pos + block.len()].copy_from_slice(block);
        write_pos += block.len();
    }
    Ok(output)
}

fn tfc_region_backup_path(backup_dir: &Path, tfc_path: &Path, offset: usize) -> PathBuf {
    let stem = tfc_path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("Textures");
    backup_dir.join(format!("{stem}_{offset}_tfc.bin"))
}

fn backup_tfc_region(
    tfc_path: &Path,
    backup_dir: &Path,
    offset: usize,
    size: usize,
) -> Result<(), String> {
    let region_backup = tfc_region_backup_path(backup_dir, tfc_path, offset);
    if region_backup.is_file() {
        return Ok(());
    }
    let full_backup = backup_dir.join(format!(
        "{}.bak",
        tfc_path.file_name().unwrap_or_default().to_string_lossy()
    ));
    let source = if full_backup.is_file() {
        full_backup
    } else {
        tfc_path.to_path_buf()
    };
    let mut file = fs::File::open(&source)
        .map_err(|error| format!("Failed to open {}: {error}", source.display()))?;
    file.seek(SeekFrom::Start(offset as u64))
        .map_err(|error| format!("Failed to seek {}: {error}", source.display()))?;
    let mut bytes = vec![0u8; size];
    file.read_exact(&mut bytes)
        .map_err(|error| format!("Failed to backup {} region: {error}", source.display()))?;
    fs::write(&region_backup, bytes).map_err(|error| {
        format!(
            "Failed to write region backup {}: {error}",
            region_backup.display()
        )
    })
}

fn write_tfc_region(
    tfc_path: &Path,
    backup_dir: &Path,
    offset: usize,
    payload: &[u8],
) -> Result<(), String> {
    backup_tfc_region(tfc_path, backup_dir, offset, payload.len())?;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .open(tfc_path)
        .map_err(|error| format!("Failed to open {}: {error}", tfc_path.display()))?;
    file.seek(SeekFrom::Start(offset as u64))
        .map_err(|error| format!("Failed to seek {}: {error}", tfc_path.display()))?;
    file.write_all(payload)
        .map_err(|error| format!("Failed to patch {}: {error}", tfc_path.display()))
}

fn append_tfc_chunk(tfc_path: &Path, backup_dir: &Path, payload: &[u8]) -> Result<usize, String> {
    let stem = tfc_path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("Textures");
    let length_backup = backup_dir.join(format!("{stem}_APPENDLEN.txt"));
    let full_backup = backup_dir.join(format!(
        "{}.bak",
        tfc_path.file_name().unwrap_or_default().to_string_lossy()
    ));
    if !length_backup.is_file() {
        let original_len = if full_backup.is_file() {
            fs::metadata(&full_backup)
        } else {
            fs::metadata(tfc_path)
        }
        .map_err(|error| format!("Failed to inspect {}: {error}", tfc_path.display()))?
        .len();
        fs::write(&length_backup, original_len.to_string()).map_err(|error| {
            format!(
                "Failed to record original length for {}: {error}",
                tfc_path.display()
            )
        })?;
    }
    let mut file = fs::OpenOptions::new()
        .write(true)
        .open(tfc_path)
        .map_err(|error| format!("Failed to open {}: {error}", tfc_path.display()))?;
    let offset = file
        .seek(SeekFrom::End(0))
        .map_err(|error| format!("Failed to seek {}: {error}", tfc_path.display()))?;
    file.write_all(payload)
        .map_err(|error| format!("Failed to append {}: {error}", tfc_path.display()))?;
    usize::try_from(offset).map_err(|_| "TFC offset exceeds this platform's limits".to_string())
}

fn restore_package_texture_regions(
    package_name: &str,
    game_dir: &Path,
    backup_dir: &Path,
    base_dir: &Path,
) -> Result<(), String> {
    let package_backup = backup_dir.join(format!("{package_name}.bak"));
    if !package_backup.is_file() {
        return Ok(());
    }
    let raw = fs::read(&package_backup)
        .map_err(|error| format!("Failed to read {}: {error}", package_backup.display()))?;
    let package = EncryptedPackage::load(raw, base_dir)?;
    let textures = parse_texture_exports(&package, game_dir)?;
    for texture in textures {
        let Some(tfc_name) = texture.tfc_name else {
            continue;
        };
        let tfc_path = game_dir.join(format!("{tfc_name}.tfc"));
        for mip in texture.mips {
            let region = tfc_region_backup_path(backup_dir, &tfc_path, mip.tfc_offset);
            if !region.is_file() {
                continue;
            }
            let bytes = fs::read(&region)
                .map_err(|error| format!("Failed to read {}: {error}", region.display()))?;
            let mut file = fs::OpenOptions::new()
                .write(true)
                .open(&tfc_path)
                .map_err(|error| format!("Failed to open {}: {error}", tfc_path.display()))?;
            file.seek(SeekFrom::Start(mip.tfc_offset as u64))
                .and_then(|_| file.write_all(&bytes))
                .map_err(|error| format!("Failed to restore {}: {error}", tfc_path.display()))?;
        }
    }
    if let Ok(entries) = fs::read_dir(backup_dir) {
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            let Some(tfc_stem) = name.strip_suffix("_APPENDLEN.txt") else {
                continue;
            };
            let Ok(length) = fs::read_to_string(&path).and_then(|value| {
                value
                    .trim()
                    .parse::<u64>()
                    .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
            }) else {
                continue;
            };
            let tfc_path = game_dir.join(format!("{tfc_stem}.tfc"));
            if tfc_path.is_file() {
                fs::OpenOptions::new()
                    .write(true)
                    .open(&tfc_path)
                    .and_then(|file| file.set_len(length))
                    .map_err(|error| {
                        format!("Failed to restore {}: {error}", tfc_path.display())
                    })?;
            }
        }
    }
    fs::copy(&package_backup, game_dir.join(package_name))
        .map_err(|error| format!("Failed to restore {package_name}: {error}"))?;
    Ok(())
}

// ============================================================================
// DECAL ITEMS - UI State
// ============================================================================

#[derive(Clone)]
pub struct DecalItem {
    pub name: String,
    pub json_path: PathBuf,
    pub preview_image: Option<Arc<[u8]>>,
    pub body_id: i32,
    pub fields: HashMap<String, PathBuf>,
}

#[derive(Clone)]
pub struct SkinInfo {
    pub id: String,
    pub name: String,
    pub upk_path: String,
}

#[derive(Clone)]
pub struct CarSkinInfo {
    pub car_key: String,
    pub car_name: String,
    pub skins: Vec<SkinInfo>,
}

#[derive(Clone)]
pub struct DecalZipEntry {
    pub body_id: i32,
    pub fields: HashMap<String, Vec<u8>>,
    pub field_labels: HashMap<String, String>,
}

// ============================================================================
// DECAL PATCHER STATE - UI State
// ============================================================================

pub struct DecalPatcherState {
    base_dir: PathBuf,
    pub decals_dir: PathBuf,
    pub decals: Vec<DecalItem>,
    pub active_decals: HashMap<String, String>,
    pub processing_target: Option<String>,
    pub progress: Option<f32>,
    pub progress_label: String,
    pub search_input: String,
    pub search_filter: String,
    pub show_applied: bool,
    pub page: usize,
    pub confirm_delete: Option<DecalItem>,
    pub restore_all_confirmed: bool,
    pub skin_dropdown_filter: String,
    pub car_skins: Vec<CarSkinInfo>,
    pub selected_car: Option<String>,
    pub selected_skin_id: Option<String>,
    pub selected_decal_name: Option<String>,
    pub catalog_error: Option<String>,
    local_tx: crossbeam_channel::Sender<DecalOp>,
    local_rx: crossbeam_channel::Receiver<DecalOp>,
}

enum DecalOp {
    Applied {
        name: String,
        active_key: String,
        target_upk: String,
    },
    Restored {
        active_key: String,
        name: String,
    },
    RestoredAll {
        count: usize,
    },
    Progress {
        fraction: f32,
        label: String,
    },
    Error(String),
}

// ============================================================================
// DECAL PATCHING - Core logic using EncryptedPackage
// ============================================================================

fn package_supports_decal_fields(
    body_id: i32,
    package_path: &Path,
    game_dir: &Path,
    base_dir: &Path,
    fields: &HashMap<String, Vec<u8>>,
) -> Result<bool, String> {
    let raw = fs::read(package_path)
        .map_err(|error| format!("Failed to read {}: {error}", package_path.display()))?;
    let package = EncryptedPackage::load(raw, base_dir)?;
    let textures = parse_texture_exports(&package, game_dir)?;
    let effective = fields
        .keys()
        .map(|field| effective_field_key_for_matching(field, fields.len()))
        .collect::<Vec<_>>();
    let diffuse = effective
        .iter()
        .filter(|field| field.contains("diffuse") && !field.contains("mask"))
        .collect::<Vec<_>>();
    Ok(if diffuse.is_empty() {
        effective
            .iter()
            .any(|field| match_texture_export(body_id, field, &textures, None, true).is_some())
    } else {
        diffuse
            .iter()
            .all(|field| match_texture_export(body_id, field, &textures, None, true).is_some())
    })
}

fn prepare_specific_decal_carrier(
    body_id: i32,
    target_upk: &str,
    donor_upks: &[String],
    game_dir: &Path,
    backup_dir: &Path,
    base_dir: &Path,
    fields: &HashMap<String, Vec<u8>>,
    progress: &dyn Fn(f32, &str),
) -> Result<bool, String> {
    let target_live = game_dir.join(target_upk);
    if package_supports_decal_fields(body_id, &target_live, game_dir, base_dir, fields)? {
        return Ok(false);
    }

    progress(0.12, "Preparing a full-colour decal carrier...");
    let target_backup = backup_dir.join(format!("{target_upk}.bak"));
    if !target_backup.is_file() {
        fs::copy(&target_live, &target_backup)
            .map_err(|error| format!("Failed to back up {target_upk}: {error}"))?;
    }
    for donor_upk in donor_upks {
        if donor_upk.eq_ignore_ascii_case(target_upk) {
            continue;
        }
        let donor_live = game_dir.join(donor_upk);
        let donor_backup = backup_dir.join(format!("{donor_upk}.bak"));
        let donor = if donor_backup.is_file() {
            &donor_backup
        } else {
            &donor_live
        };
        if !donor.is_file()
            || !package_supports_decal_fields(body_id, donor, game_dir, base_dir, fields)
                .unwrap_or(false)
        {
            continue;
        }
        crate::cosmetic_upk::patch_for_target(
            donor,
            &target_backup,
            &target_live,
            base_dir,
            None,
            None,
        )
        .map_err(|error| {
            format!("Failed to prepare {target_upk} from carrier {donor_upk}: {error}")
        })?;
        progress(0.2, &format!("Using {donor_upk} as the decal carrier..."));
        return Ok(true);
    }
    Err(format!(
        "{target_upk} is a colour-mask decal and no compatible full-colour carrier UPK was found for this car"
    ))
}

fn patch_decal_on_skin(
    body_id: i32,
    target_candidates: &[(String, bool)],
    game_dir: &Path,
    backup_dir: &Path,
    base_dir: &Path,
    png_data_map: &HashMap<String, Vec<u8>>,
    field_labels: &HashMap<String, String>,
    use_live_package: bool,
    force_append: bool,
    progress: &dyn Fn(f32, &str),
) -> Result<(String, Vec<String>), String> {
    load_upk_keys(base_dir)?;
    progress(0.08, "Loading UPK keys...");
    fs::create_dir_all(backup_dir)
        .map_err(|e| format!("Failed to create backup directory: {e}"))?;

    let total_field_count = png_data_map.len();
    let mut selected = None;
    let mut failures = Vec::new();
    for (index, (candidate, skin_specific)) in target_candidates.iter().enumerate() {
        let upk_path = game_dir.join(candidate);
        if !upk_path.is_file() {
            failures.push(format!("{candidate}: file not found"));
            continue;
        }
        progress(
            0.12 + index as f32 * 0.12,
            &format!("Decrypting and decompressing {candidate}..."),
        );
        let backup = backup_dir.join(format!("{candidate}.bak"));
        let source = if !use_live_package && backup.is_file() {
            &backup
        } else {
            &upk_path
        };
        let raw = match fs::read(source) {
            Ok(raw) => raw,
            Err(error) => {
                failures.push(format!("{candidate}: {error}"));
                continue;
            }
        };
        let package = match EncryptedPackage::load(raw, base_dir) {
            Ok(package) => package,
            Err(error) => {
                failures.push(format!("{candidate}: {error}"));
                continue;
            }
        };
        progress(
            0.16 + index as f32 * 0.12,
            &format!("Extracting texture BulkData from {candidate}..."),
        );
        let textures = match parse_texture_exports(&package, game_dir) {
            Ok(textures) => textures,
            Err(error) => {
                failures.push(format!("{candidate}: {error}"));
                continue;
            }
        };
        let effective_fields: Vec<String> = png_data_map
            .keys()
            .map(|field_key| effective_field_key_for_matching(field_key, total_field_count))
            .collect();
        let diffuse_fields: Vec<&String> = effective_fields
            .iter()
            .filter(|field| field.contains("diffuse") && !field.contains("mask"))
            .collect();
        // A mask/normal match must not make us choose a package that lacks the
        // actual colour diffuse. This was selecting Body_Octane_SF because of
        // Body_Octane_Bevel_RGB, then failing on oct_diffuse.png instead of
        // continuing to Startup's pinned Pepe_Body_D.
        let valid = if diffuse_fields.is_empty() {
            effective_fields.iter().any(|effective| {
                match_texture_export(body_id, effective, &textures, None, *skin_specific).is_some()
            })
        } else {
            diffuse_fields.iter().all(|effective| {
                match_texture_export(body_id, effective, &textures, None, *skin_specific).is_some()
            })
        };
        if valid {
            selected = Some((
                candidate.clone(),
                upk_path,
                backup,
                package,
                textures,
                *skin_specific,
            ));
            break;
        }
        failures.push(format!(
            "{candidate}: no matching decal texture (exports: {})",
            textures
                .iter()
                .map(|texture| texture.export_name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    let Some((target_upk, upk_path, backup_upk, mut package, textures, skin_specific)) = selected
    else {
        return Err(format!(
            "No compatible decal texture package was found. {}",
            failures.join("; ")
        ));
    };

    if !backup_upk.exists() {
        fs::copy(&upk_path, &backup_upk)
            .map_err(|e| format!("Failed to backup {}: {}", target_upk, e))?;
    }
    progress(0.42, &format!("Using {target_upk}..."));
    progress(0.42, "Finding decal textures...");

    let mut patched_fields = Vec::new();
    let mut patched_exports = HashSet::new();
    let mut diffuse_patched_mips = HashSet::new();
    let mut diffuse_export_name = None::<String>;

    for (field_index, (field_key, png_bytes)) in png_data_map.iter().enumerate() {
        let label = field_labels.get(field_key).unwrap_or(field_key);
        let effective_key = effective_field_key_for_matching(field_key, total_field_count);
        let Some(texture) =
            match_texture_export(body_id, &effective_key, &textures, None, skin_specific)
        else {
            progress(
                0.45 + 0.35 * (field_index as f32 / png_data_map.len().max(1) as f32),
                &format!("Skipping {label}: no matching texture export"),
            );
            continue;
        };
        progress(
            0.45 + 0.35 * (field_index as f32 / png_data_map.len().max(1) as f32),
            &format!("Patching {label}..."),
        );
        if !patched_exports.insert(texture.export_name.to_ascii_lowercase()) {
            continue;
        }
        let tfc_name = texture.tfc_name.as_ref().ok_or_else(|| {
            format!(
                "'{}' matched '{}' but its TFC name could not be resolved",
                label, texture.export_name
            )
        })?;
        let tfc_path = game_dir.join(format!("{tfc_name}.tfc"));
        if !tfc_path.exists() {
            return Err(format!(
                "{} was not found for texture '{}'",
                tfc_path.display(),
                texture.export_name
            ));
        }
        let source = image::load_from_memory(png_bytes)
            .map_err(|error| format!("Failed to load '{}': {error}", label))?
            .to_rgba8();
        let force_opaque_alpha = effective_key.contains("diffuse")
            && !effective_key.contains("mask")
            && !texture
                .export_name
                .to_ascii_lowercase()
                .ends_with("_basecolor");
        let mut patched_mips = 0usize;

        for (mip_index, mip) in texture.mips.iter().enumerate() {
            let side = texture_mip_side(mip);
            let valid_size = match mip.format {
                TextureFormat::Dxt1 => side * side == mip.disk_size * 2,
                TextureFormat::Dxt5 => side * side == mip.disk_size,
                TextureFormat::Bgra8 => side * side * 4 == mip.disk_size,
            };
            if side == 0 || !valid_size {
                continue;
            }
            let resized = resize_rgba_isolated(&source, side as u32, side as u32);
            let encoded = match mip.format {
                TextureFormat::Dxt1 => image_to_dxt1(&resized, side, side),
                TextureFormat::Dxt5 => {
                    image_to_dxt5_with_alpha(&resized, side, side, force_opaque_alpha)
                }
                TextureFormat::Bgra8 => image_to_bgra8(&resized),
            };
            if encoded.len() != mip.disk_size {
                return Err(format!(
                    "{} mip size mismatch: {} != {}",
                    texture.export_name,
                    encoded.len(),
                    mip.disk_size
                ));
            }
            let block_count = mip.disk_size.div_ceil(131_072);
            let available_compressed = mip
                .memory_size
                .checked_sub(16 + block_count * 8)
                .ok_or("Invalid TFC mip allocation")?;
            match (!force_append)
                .then(|| build_tfc_chunk(&encoded, Some(available_compressed)))
                .transpose()
            {
                Ok(Some(chunk))
                    if mip.tfc_offset.checked_add(chunk.len()).is_some_and(|end| {
                        fs::metadata(&tfc_path)
                            .map(|metadata| end as u64 <= metadata.len())
                            .unwrap_or(false)
                    }) =>
                {
                    write_tfc_region(&tfc_path, backup_dir, mip.tfc_offset, &chunk)?;
                }
                _ => {
                    let chunk = build_tfc_chunk(&encoded, None)?;
                    let new_offset = append_tfc_chunk(&tfc_path, backup_dir, &chunk)?;
                    if mip.legacy_offset {
                        let value = u32::try_from(new_offset)
                            .map_err(|_| "TFC file exceeds legacy 32-bit offsets")?;
                        package.logical_data[mip.offset_field..mip.offset_field + 4]
                            .copy_from_slice(&value.to_le_bytes());
                        package.mark_modified_range(mip.offset_field, mip.offset_field + 4)?;
                    } else {
                        package.logical_data[mip.offset_field..mip.offset_field + 8]
                            .copy_from_slice(&(new_offset as i64).to_le_bytes());
                        package.mark_modified_range(mip.offset_field, mip.offset_field + 8)?;
                    }
                    let chunk_len =
                        i32::try_from(chunk.len()).map_err(|_| "TFC chunk is too large")?;
                    package.logical_data[mip.memory_size_field..mip.memory_size_field + 4]
                        .copy_from_slice(&chunk_len.to_le_bytes());
                    package
                        .mark_modified_range(mip.memory_size_field, mip.memory_size_field + 4)?;
                }
            }
            patched_mips += 1;
            if effective_key.contains("diffuse") {
                diffuse_patched_mips.insert(mip_index);
                diffuse_export_name = Some(texture.export_name.clone());
            }
        }
        if patched_mips == 0 {
            return Err(format!(
                "'{}' matched '{}' but none of its mips could be patched",
                label, texture.export_name
            ));
        }
        patched_fields.push(label.clone());
    }

    if patched_fields.is_empty() {
        return Err("No textures were patched successfully".to_string());
    }

    // Direct port of C# PatchBlankSkin. Body diffuse decals need the matching
    // BlankSkin mip chain cleared or the paint material washes/obscures the
    // full-colour texture in game.
    if let Some(diffuse_name) = diffuse_export_name.as_deref() {
        let lower = diffuse_name.to_ascii_lowercase();
        let body_diffuse = !lower.ends_with("_basecolor")
            && !lower.starts_with("skin_")
            && !lower.contains("_skin_");
        if body_diffuse && !diffuse_patched_mips.is_empty() {
            let diffuse_sides = textures
                .iter()
                .find(|texture| texture.export_name == diffuse_name)
                .map(|texture| {
                    texture
                        .mips
                        .iter()
                        .map(texture_mip_side)
                        .collect::<Vec<_>>()
                });
            let mut blanks: Vec<&TextureExport> = textures
                .iter()
                .filter(|texture| {
                    texture
                        .export_name
                        .to_ascii_lowercase()
                        .contains("blankskin")
                })
                .collect();
            if let Some(pinned) = startup_upk_field_override(body_id, "blankskin") {
                if let Some(texture) = blanks
                    .iter()
                    .copied()
                    .find(|texture| texture.export_name.eq_ignore_ascii_case(pinned))
                {
                    blanks = vec![texture];
                }
            }
            let blank = diffuse_sides
                .as_ref()
                .and_then(|sides| {
                    blanks.iter().copied().find(|texture| {
                        texture
                            .mips
                            .iter()
                            .map(texture_mip_side)
                            .eq(sides.iter().copied())
                    })
                })
                .or_else(|| blanks.first().copied());
            if let Some(blank) = blank {
                progress(0.82, "Patching BlankSkin...");
                let tfc_name = blank.tfc_name.as_ref().ok_or_else(|| {
                    format!("BlankSkin export '{}' has no TFC name", blank.export_name)
                })?;
                let tfc_path = game_dir.join(format!("{tfc_name}.tfc"));
                for mip_index in diffuse_patched_mips.iter().copied() {
                    let Some(mip) = blank.mips.get(mip_index) else {
                        continue;
                    };
                    if mip.format != TextureFormat::Dxt5 {
                        continue;
                    }
                    let side = texture_mip_side(mip);
                    if side * side != mip.disk_size {
                        continue;
                    }
                    let block_count = mip.disk_size.div_ceil(131_072);
                    let Some(available) = mip.memory_size.checked_sub(16 + block_count * 8) else {
                        continue;
                    };
                    let mut black = vec![0u8; mip.disk_size];
                    for block in black.chunks_exact_mut(BLACK_DXT5_BLOCK.len()) {
                        block.copy_from_slice(&BLACK_DXT5_BLOCK);
                    }
                    if let Ok(chunk) = build_tfc_chunk(&black, Some(available)) {
                        write_tfc_region(&tfc_path, backup_dir, mip.tfc_offset, &chunk)?;
                    }
                }
            }
        }
    }

    let final_out = if package.modified_chunks.is_empty() {
        package.raw.clone()
    } else {
        repack_package(&package)?
    };
    progress(0.9, "Encrypting patched UPK...");
    fs::write(&upk_path, &final_out).map_err(|e| format!("Failed to write patched UPK: {}", e))?;
    progress(1.0, "Decal patch complete");
    Ok((target_upk, patched_fields))
}

// ============================================================================
// DECAL PATCHER STATE IMPL
// ============================================================================

fn restore_decal_candidates(
    target_candidates: &[String],
    cooked_pc: &Path,
    backups_dir: &Path,
) -> Result<(), String> {
    let mut restored_package = false;
    for target_upk in target_candidates {
        let backup_upk = backups_dir.join(format!("{target_upk}.bak"));
        if backup_upk.is_file() {
            fs::copy(&backup_upk, cooked_pc.join(target_upk))
                .map_err(|error| format!("Failed to restore {target_upk}: {error}"))?;
            restored_package = true;
        }
    }
    if !restored_package {
        return Err("No decal package backup was found".to_string());
    }
    let entries: Vec<PathBuf> = fs::read_dir(backups_dir)
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .map(|entry| entry.path())
                .collect()
        })
        .unwrap_or_default();
    let mut restored_regions = false;
    for path in &entries {
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let Some(stem) = name.strip_suffix("_tfc.bin") else {
            continue;
        };
        let Some((tfc_stem, offset)) = stem.rsplit_once('_') else {
            continue;
        };
        let Ok(offset) = offset.parse::<u64>() else {
            continue;
        };
        let tfc_path = cooked_pc.join(format!("{tfc_stem}.tfc"));
        let bytes = fs::read(path)
            .map_err(|error| format!("Failed to read {}: {error}", path.display()))?;
        let mut file = fs::OpenOptions::new()
            .write(true)
            .open(&tfc_path)
            .map_err(|error| format!("Failed to open {}: {error}", tfc_path.display()))?;
        file.seek(SeekFrom::Start(offset))
            .and_then(|_| file.write_all(&bytes))
            .map_err(|error| format!("Failed to restore {}: {error}", tfc_path.display()))?;
        restored_regions = true;
    }
    for path in &entries {
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let Some(stem) = name.strip_suffix("_APPENDLEN.txt") else {
            continue;
        };
        let Ok(length) = fs::read_to_string(path).and_then(|value| {
            value
                .trim()
                .parse::<u64>()
                .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
        }) else {
            continue;
        };
        let tfc_path = cooked_pc.join(format!("{stem}.tfc"));
        fs::OpenOptions::new()
            .write(true)
            .open(&tfc_path)
            .and_then(|file| file.set_len(length))
            .map_err(|error| format!("Failed to truncate {}: {error}", tfc_path.display()))?;
        restored_regions = true;
    }
    // Compatibility with backups created by the previous full-file Rust
    // implementation. New applies use surgical region backups above.
    if !restored_regions {
        for path in &entries {
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if let Some(original_name) = name.strip_suffix(".tfc.bak") {
                fs::copy(path, cooked_pc.join(format!("{original_name}.tfc")))
                    .map_err(|error| format!("Failed to restore {original_name}.tfc: {error}"))?;
            }
        }
    }
    Ok(())
}

fn restore_all_decal_files(
    target_candidates: &[String],
    cooked_pc: &Path,
    backups_dir: &Path,
) -> Result<usize, String> {
    let package_count = target_candidates
        .iter()
        .filter(|target| backups_dir.join(format!("{target}.bak")).is_file())
        .count();
    let tfc_count = fs::read_dir(backups_dir)
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .filter(|entry| {
                    entry
                        .file_name()
                        .to_str()
                        .is_some_and(|name| name.ends_with(".tfc.bak"))
                })
                .count()
        })
        .unwrap_or_default();
    restore_decal_candidates(target_candidates, cooked_pc, backups_dir)?;
    Ok(package_count + tfc_count)
}

fn decal_catalog_id(value: Option<&Value>) -> Option<i32> {
    value.and_then(|value| {
        value
            .as_i64()
            .and_then(|id| i32::try_from(id).ok())
            .or_else(|| value.as_str()?.parse().ok())
    })
}

fn normalize_catalog_name(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

impl DecalPatcherState {
    pub fn new(base_dir: &Path, config: &Config) -> Self {
        let decals_dir = base_dir.join("decals");
        let _ = fs::create_dir_all(&decals_dir);

        let (local_tx, local_rx) = crossbeam_channel::unbounded();

        let mut state = Self {
            base_dir: base_dir.to_path_buf(),
            decals_dir,
            decals: Vec::new(),
            active_decals: config.patcher.active_decals.clone(),
            processing_target: None,
            progress: None,
            progress_label: String::new(),
            search_input: String::new(),
            search_filter: String::new(),
            show_applied: false,
            page: 0,
            confirm_delete: None,
            restore_all_confirmed: false,
            car_skins: Vec::new(),
            selected_car: None,
            selected_skin_id: None,
            selected_decal_name: None,
            catalog_error: None,
            skin_dropdown_filter: String::new(),
            local_tx,
            local_rx,
        };

        state.load_car_skins();
        state.refresh_decals();
        state
    }

    fn find_skin(&self, car_key: &str, skin_id: &str) -> Result<SkinInfo, String> {
        let car = self
            .car_skins
            .iter()
            .find(|c| c.car_key == car_key)
            .ok_or_else(|| format!("Car '{}' not found", car_key))?;

        let skin = car
            .skins
            .iter()
            .find(|s| s.id == skin_id)
            .ok_or_else(|| format!("Skin '{}' not found for car '{}'", skin_id, car_key))?;

        Ok(skin.clone())
    }

    pub fn target_display_name(&self, active_key: &str) -> String {
        let Some((car_key, skin_id)) = active_key.split_once('|') else {
            return active_key.to_string();
        };
        self.car_skins
            .iter()
            .find(|car| car.car_key == car_key)
            .and_then(|car| {
                car.skins
                    .iter()
                    .find(|skin| skin.id == skin_id)
                    .map(|skin| format!("{} · {}", car.car_name, skin.name))
            })
            .unwrap_or_else(|| active_key.to_string())
    }

    pub fn load_car_skins(&mut self) {
        self.car_skins.clear();
        self.catalog_error = None;

        let json: Value = match serde_json::from_str(SKINS_CATALOG) {
            Ok(j) => j,
            Err(e) => {
                self.catalog_error = Some(format!("Failed to parse skins.json: {}", e));
                return;
            }
        };

        let cars = match json.get("cars").and_then(|v| v.as_object()) {
            Some(c) => c,
            None => {
                self.catalog_error = Some("No 'cars' object found in skins.json".to_string());
                return;
            }
        };

        for (car_key, car_data) in cars {
            let skins = match car_data.get("skins").and_then(|v| v.as_array()) {
                Some(s) => s,
                None => continue,
            };

            let car_name = self.format_car_name(car_key);
            let mut skin_list = Vec::new();

            for skin in skins {
                let id = match skin.get("id").and_then(|v| v.as_str()) {
                    Some(id) => id.to_string(),
                    None => continue,
                };
                let name = match skin.get("name").and_then(|v| v.as_str()) {
                    Some(n) => n.to_string(),
                    None => continue,
                };
                let upk_path = match skin.get("upk_path").and_then(|v| v.as_str()) {
                    Some(s) => s.to_string(),
                    None => continue,
                };
                skin_list.push(SkinInfo { id, name, upk_path });
            }

            if !skin_list.is_empty() {
                self.car_skins.push(CarSkinInfo {
                    car_key: car_key.to_string(),
                    car_name,
                    skins: skin_list,
                });
            }
        }

        self.car_skins.sort_by(|a, b| a.car_name.cmp(&b.car_name));
        if !self.car_skins.is_empty() {
            self.catalog_error = None;
        }
    }

    fn format_car_name(&self, raw: &str) -> String {
        raw.split('_')
            .map(|w| {
                let mut c = w.chars();
                match c.next() {
                    None => String::new(),
                    Some(f) => {
                        f.to_uppercase().collect::<String>() + c.as_str().to_lowercase().as_str()
                    }
                }
            })
            .collect::<Vec<_>>()
            .join(" ")
    }

    pub fn refresh_decals(&mut self) {
        self.decals.clear();
        if !self.decals_dir.exists() {
            return;
        }

        let mut to_visit = vec![self.decals_dir.clone()];
        while let Some(dir) = to_visit.pop() {
            if let Ok(entries) = fs::read_dir(&dir) {
                for entry in entries.filter_map(|e| e.ok()) {
                    let path = entry.path();
                    if path.is_dir() {
                        to_visit.push(path);
                    } else if path.extension().and_then(|s| s.to_str()) == Some("json") {
                        self.parse_decal_json(&path);
                    }
                }
            }
        }
    }

    fn parse_decal_json(&mut self, json_path: &Path) {
        if let Ok(content) = fs::read_to_string(json_path) {
            if let Ok(parsed) = serde_json::from_str::<Value>(&content) {
                if let Some(obj) = parsed.as_object() {
                    for (pack_name, pack_data) in obj {
                        let body_id = pack_data
                            .get("BodyID")
                            .or(pack_data.get("body_id"))
                            .and_then(|v| v.as_i64())
                            .unwrap_or(0) as i32;

                        let skin_id = pack_data
                            .get("SkinID")
                            .or(pack_data.get("skin_id"))
                            .and_then(|v| v.as_i64())
                            .map(|v| v as i32);

                        let target_upk = pack_data
                            .get("TargetUpk")
                            .or(pack_data.get("target_upk"))
                            .or(pack_data.get("upk_path"))
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string());

                        let (car_name, skin_name) = self.resolve_display_names(body_id, skin_id);

                        let mut fields = HashMap::new();
                        let mut preview_image = None;

                        let body_section = pack_data.get("Body").or(pack_data.get("Chassis"));
                        if let Some(body) = body_section.and_then(|v| v.as_object()) {
                            let parent = json_path.parent().unwrap_or(Path::new(""));
                            for (field_key, field_value) in body {
                                if let Some(png_name) = field_value.as_str() {
                                    if png_name.ends_with(".png") {
                                        let png_path = parent.join(png_name);
                                        if png_path.exists() {
                                            if preview_image.is_none() {
                                                if let Ok(bytes) = fs::read(&png_path) {
                                                    preview_image = Some(Arc::from(bytes));
                                                }
                                            }
                                            fields.insert(field_key.clone(), png_path);
                                        }
                                    }
                                }
                            }
                        }

                        if !fields.is_empty() {
                            let display_name = if let Some(skin) = skin_name.as_ref() {
                                format!("{} · {}", car_name, skin)
                            } else if let Some(upk) = target_upk.as_ref() {
                                format!("{} · {}", car_name, upk)
                            } else {
                                format!("{} · {}", car_name, pack_name)
                            };

                            self.decals.push(DecalItem {
                                name: display_name,
                                json_path: json_path.to_path_buf(),
                                preview_image,
                                body_id,
                                fields,
                            });
                        }
                    }
                }
            }
        }
    }

    fn resolve_display_names(
        &self,
        body_id: i32,
        skin_id: Option<i32>,
    ) -> (String, Option<String>) {
        let car_name = self.lookup_body_name(body_id);
        let skin_name = skin_id.and_then(|id| self.lookup_skin_name(id));
        (car_name, skin_name)
    }

    fn lookup_body_name(&self, body_id: i32) -> String {
        if let Some(json) = self.load_bodies_catalog() {
            if let Some(bodies) = json.get("bodies").and_then(Value::as_array) {
                for body in bodies {
                    if decal_catalog_id(body.get("id")) == Some(body_id) {
                        if let Some(name) = body.get("name").and_then(Value::as_str) {
                            return name.to_string();
                        }
                    }
                }
            }
        }
        format!("Body {}", body_id)
    }

    fn car_key_for_body_id(&self, body_id: i32) -> Option<String> {
        let body_name = self.lookup_body_name(body_id);
        let normalized_body = normalize_catalog_name(&body_name);
        self.car_skins
            .iter()
            .find(|car| {
                normalize_catalog_name(&car.car_name) == normalized_body
                    || normalize_catalog_name(&car.car_key) == normalized_body
            })
            .map(|car| car.car_key.clone())
    }

    fn selected_decal_car(&self) -> Option<&CarSkinInfo> {
        let selected_name = self.selected_decal_name.as_deref()?;
        let decal = self
            .decals
            .iter()
            .find(|decal| decal.name == selected_name)?;
        let car_key = self.car_key_for_body_id(decal.body_id)?;
        self.car_skins.iter().find(|car| car.car_key == car_key)
    }

    fn lookup_body_upk(&self, body_id: i32) -> Option<String> {
        let json = self.load_bodies_catalog()?;
        json.get("bodies")?
            .as_array()?
            .iter()
            .find(|body| decal_catalog_id(body.get("id")) == Some(body_id))?
            .get("upk_path")?
            .as_str()
            .map(str::to_string)
    }

    fn load_bodies_catalog(&self) -> Option<Value> {
        for path in [
            self.base_dir.join("bodies.json"),
            self.base_dir.join("assets/catalogs/bodies.json"),
        ] {
            if let Ok(data) = fs::read_to_string(path) {
                if let Ok(json) = serde_json::from_str(&data) {
                    return Some(json);
                }
            }
        }
        serde_json::from_str(include_str!("../../assets/catalogs/bodies.json")).ok()
    }

    fn lookup_skin_name(&self, skin_id: i32) -> Option<String> {
        if let Ok(json) = serde_json::from_str::<Value>(SKINS_CATALOG) {
            if let Some(cars) = json.get("cars").and_then(|v| v.as_object()) {
                for car_data in cars.values() {
                    if let Some(skins) = car_data.get("skins").and_then(|v| v.as_array()) {
                        for skin in skins {
                            if let Some(id) = skin.get("id").and_then(|v| v.as_str()) {
                                if id == skin_id.to_string() {
                                    return skin
                                        .get("name")
                                        .and_then(|v| v.as_str())
                                        .map(|s| s.to_string());
                                }
                            }
                        }
                    }
                }
            }
        }
        None
    }

    fn validate_key_file(&self) -> Result<(), String> {
        load_upk_keys(&self.base_dir).map(|_| ())
    }

    fn build_backend_entry(&self, decal: &DecalItem) -> DecalZipEntry {
        let mut fields_map = HashMap::new();
        let mut field_labels = HashMap::new();
        for (key, path) in &decal.fields {
            if let Ok(bytes) = fs::read(path) {
                fields_map.insert(key.clone(), bytes);
                field_labels.insert(
                    key.clone(),
                    path.file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .to_string(),
                );
            }
        }

        DecalZipEntry {
            body_id: decal.body_id,
            fields: fields_map,
            field_labels,
        }
    }

    pub fn apply_decal_to_skin(
        &mut self,
        decal_name: &str,
        car_key: &str,
        skin_id: &str,
        cooked_pc: &Path,
        backups_dir: &Path,
        tx: &Sender<AppMsg>,
        ctx: &egui::Context,
    ) -> Result<(), String> {
        self.validate_key_file()?;

        let decal_name_owned = decal_name.to_string();
        let car_key_owned = car_key.to_string();
        let skin_id_owned = skin_id.to_string();

        let skin_info = self.find_skin(car_key, skin_id)?;

        let decal = self
            .decals
            .iter()
            .find(|d| d.name == decal_name)
            .ok_or_else(|| format!("Decal '{}' not found", decal_name))?;
        let expected_car = self.car_key_for_body_id(decal.body_id).ok_or_else(|| {
            format!(
                "BodyID {} does not match a car in bodies.json and skins.json",
                decal.body_id
            )
        })?;
        if expected_car != car_key {
            return Err(format!(
                "'{}' is for {}, not {}",
                decal_name,
                self.lookup_body_name(decal.body_id),
                car_key
            ));
        }
        // The imported decal's BodyID constrains which car can be targeted, while
        // the selected catalog skin is the concrete decal package to replace.
        // Never fall back to Body_*.upk or Startup.upk here: those textures are
        // shared by the whole car and are not a skin/decal replacement.
        let target_candidates = vec![(skin_info.upk_path.clone(), true)];
        let mut legacy_shared_targets = Vec::new();
        if let Some(body_upk) = self.lookup_body_upk(decal.body_id) {
            legacy_shared_targets.push(body_upk.clone());
            if !body_upk.eq_ignore_ascii_case("Startup.upk") {
                legacy_shared_targets.push("Startup.upk".to_string());
            }
        }

        let car_info = self
            .car_skins
            .iter()
            .find(|c| c.car_key == car_key)
            .ok_or_else(|| format!("Car '{}' not found", car_key))?;
        let mut donor_skins = car_info.skins.clone();
        donor_skins.sort_by_key(|skin| {
            let name = skin.name.to_ascii_lowercase();
            let preferred =
                name.contains("esports") || name.contains("(home)") || name.contains("(away)");
            (!preferred, skin.name.clone())
        });
        let donor_upks = donor_skins
            .into_iter()
            .map(|skin| skin.upk_path)
            .collect::<Vec<_>>();
        let previous_targets = self
            .active_decals
            .keys()
            .filter_map(|active_key| active_key.split_once('|'))
            .filter_map(|(active_car, active_skin)| self.find_skin(active_car, active_skin).ok())
            .map(|skin| skin.upk_path)
            .collect::<HashSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();

        self.processing_target = Some(format!("{} -> {}", decal_name, skin_info.name));

        let entry = self.build_backend_entry(decal);

        let _ = tx.send(AppMsg::Log(format!(
            "[Decals] Applying '{}' for {}'s {} skin (checking: {})",
            decal_name,
            car_info.car_name,
            skin_info.name,
            target_candidates
                .iter()
                .map(|(target, _)| target.as_str())
                .collect::<Vec<_>>()
                .join(" -> ")
        )));

        // Spawn thread for patching
        let decal_name_clone = decal_name_owned.clone();
        let car_key_clone = car_key_owned.clone();
        let skin_id_clone = skin_id_owned.clone();
        let cooked_clone = cooked_pc.to_path_buf();
        let backups_clone = backups_dir.to_path_buf();
        let base_dir_clone = self.base_dir.clone();
        let fields_clone = entry.fields.clone();
        let field_labels_clone = entry.field_labels.clone();
        let target_candidates_clone = target_candidates.clone();
        let legacy_shared_targets_clone = legacy_shared_targets;
        let selected_skin_upk = skin_info.upk_path.clone();
        let donor_upks_clone = donor_upks;
        let previous_targets_clone = previous_targets;
        let local_tx = self.local_tx.clone();
        let ctx_clone = ctx.clone();

        std::thread::spawn(move || {
            let progress_tx = local_tx.clone();
            let result = legacy_shared_targets_clone
                .iter()
                .filter(|target| backups_clone.join(format!("{target}.bak")).is_file())
                .try_for_each(|target| {
                    restore_package_texture_regions(
                        target,
                        &cooked_clone,
                        &backups_clone,
                        &base_dir_clone,
                    )
                })
                .and_then(|()| {
                    previous_targets_clone
                        .iter()
                        .filter(|target| backups_clone.join(format!("{target}.bak")).is_file())
                        .try_for_each(|target| {
                            restore_package_texture_regions(
                                target,
                                &cooked_clone,
                                &backups_clone,
                                &base_dir_clone,
                            )
                        })
                })
                .and_then(|()| {
                    restore_package_texture_regions(
                        &selected_skin_upk,
                        &cooked_clone,
                        &backups_clone,
                        &base_dir_clone,
                    )
                })
                .and_then(|()| {
                    prepare_specific_decal_carrier(
                        entry.body_id,
                        &selected_skin_upk,
                        &donor_upks_clone,
                        &cooked_clone,
                        &backups_clone,
                        &base_dir_clone,
                        &fields_clone,
                        &|fraction, label| {
                            let _ = progress_tx.send(DecalOp::Progress {
                                fraction,
                                label: label.to_string(),
                            });
                        },
                    )
                })
                .and_then(|carrier_installed| {
                    patch_decal_on_skin(
                        entry.body_id,
                        &target_candidates_clone,
                        &cooked_clone,
                        &backups_clone,
                        &base_dir_clone,
                        &fields_clone,
                        &field_labels_clone,
                        true,
                        carrier_installed,
                        &|fraction, label| {
                            let _ = progress_tx.send(DecalOp::Progress {
                                fraction,
                                label: label.to_string(),
                            });
                        },
                    )
                });

            match result {
                Ok((target_upk, _patched_fields)) => {
                    let _ = local_tx.send(DecalOp::Applied {
                        name: decal_name_clone,
                        active_key: format!("{}|{}", car_key_clone, skin_id_clone),
                        target_upk,
                    });
                }
                Err(e) => {
                    let _ = local_tx.send(DecalOp::Error(e));
                }
            }
            ctx_clone.request_repaint();
        });

        Ok(())
    }

    pub fn restore_decal_from_skin(
        &mut self,
        car_key: &str,
        skin_id: &str,
        cooked_pc: &Path,
        backups_dir: &Path,
        _tx: &Sender<AppMsg>,
        ctx: &egui::Context,
    ) -> Result<(), String> {
        let active_key = format!("{}|{}", car_key, skin_id);
        let decal_name = self
            .active_decals
            .get(&active_key)
            .cloned()
            .ok_or_else(|| "No decal applied to this skin".to_string())?;

        // Find the skin info to get the target UPK
        let skin_info = self.find_skin(car_key, skin_id)?;
        let target_candidates = vec![skin_info.upk_path];

        self.processing_target = Some(format!("Restoring {}", decal_name));
        self.progress = Some(0.05);
        self.progress_label = "Restoring original files...".to_string();
        let target_candidates_clone = target_candidates.clone();
        let cooked_clone = cooked_pc.to_path_buf();
        let backups_clone = backups_dir.to_path_buf();
        let base_dir_clone = self.base_dir.clone();
        let active_key_clone = active_key.clone();
        let decal_name_clone = decal_name.clone();
        let local_tx = self.local_tx.clone();
        let ctx_clone = ctx.clone();
        std::thread::spawn(move || {
            let result = target_candidates_clone.iter().try_for_each(|target| {
                let backup = backups_clone.join(format!("{target}.bak"));
                if !backup.is_file() {
                    return Err(format!("No decal package backup was found for {target}"));
                }
                restore_package_texture_regions(
                    target,
                    &cooked_clone,
                    &backups_clone,
                    &base_dir_clone,
                )
            });
            match result {
                Ok(()) => {
                    let _ = local_tx.send(DecalOp::Restored {
                        active_key: active_key_clone,
                        name: decal_name_clone,
                    });
                }
                Err(error) => {
                    let _ = local_tx.send(DecalOp::Error(error));
                }
            }
            ctx_clone.request_repaint();
        });
        Ok(())
    }

    pub fn restore_all_decals(
        &mut self,
        cooked_pc: &Path,
        backups_dir: &Path,
        _tx: &Sender<AppMsg>,
        ctx: &egui::Context,
    ) -> Result<(), String> {
        let mut target_candidates = HashSet::new();
        for active_key in self.active_decals.keys() {
            let Some((car_key, skin_id)) = active_key.split_once('|') else {
                continue;
            };
            if let Ok(skin) = self.find_skin(car_key, skin_id) {
                target_candidates.insert(skin.upk_path);
            }
        }
        // Include shared packages touched by older Rust builds so Restore All
        // also migrates those accidental whole-car patches back to originals.
        for decal_name in self.active_decals.values() {
            let Some(decal) = self.decals.iter().find(|decal| decal.name == *decal_name) else {
                continue;
            };
            if let Some(body_upk) = self.lookup_body_upk(decal.body_id) {
                if backups_dir.join(format!("{body_upk}.bak")).is_file() {
                    target_candidates.insert(body_upk);
                }
                if backups_dir.join("Startup.upk.bak").is_file() {
                    target_candidates.insert("Startup.upk".to_string());
                }
            }
        }
        if target_candidates.is_empty() {
            return Err("No active decals to restore".to_string());
        }

        self.processing_target = Some("Global_Restore".to_string());
        self.progress = Some(0.05);
        self.progress_label = "Restoring original files...".to_string();
        let cooked_clone = cooked_pc.to_path_buf();
        let backups_clone = backups_dir.to_path_buf();
        let target_candidates: Vec<String> = target_candidates.into_iter().collect();
        let local_tx = self.local_tx.clone();
        let ctx_clone = ctx.clone();
        std::thread::spawn(move || {
            match restore_all_decal_files(&target_candidates, &cooked_clone, &backups_clone) {
                Ok(count) => {
                    let _ = local_tx.send(DecalOp::RestoredAll { count });
                }
                Err(error) => {
                    let _ = local_tx.send(DecalOp::Error(error));
                }
            }
            ctx_clone.request_repaint();
        });
        Ok(())
    }

    pub fn import_zip(&mut self, zip_path: &Path, tx: &Sender<AppMsg>) -> Result<(), String> {
        let _ = tx.send(AppMsg::Log("[Decals] Extracting ZIP...".to_string()));

        let file = fs::File::open(zip_path).map_err(|e| e.to_string())?;
        let mut archive = zip::ZipArchive::new(file).map_err(|e| e.to_string())?;

        let zip_stem = zip_path.file_stem().unwrap().to_string_lossy().to_string();
        let temp_dir = self.decals_dir.join(format!("temp_{}", zip_stem));

        fs::create_dir_all(&temp_dir).map_err(|e| e.to_string())?;
        archive.extract(&temp_dir).map_err(|e| e.to_string())?;

        let entries: Vec<_> = fs::read_dir(&temp_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .collect();

        if entries.len() == 1 && entries[0].file_type().unwrap().is_dir() {
            let inner_dir = entries[0].path();
            let dest = self.decals_dir.join(inner_dir.file_name().unwrap());
            let _ = fs::rename(&inner_dir, &dest);
            let _ = fs::remove_dir_all(&temp_dir);
        } else {
            let dest = self.decals_dir.join(&zip_stem);
            let _ = fs::rename(&temp_dir, &dest);
        }

        self.refresh_decals();
        let _ = tx.send(AppMsg::Log("[Decals] Imported successfully!".to_string()));
        Ok(())
    }

    pub fn delete_decal(
        &mut self,
        decal_name: &str,
        tx: &Sender<AppMsg>,
        config: &mut Config,
    ) -> Result<(), String> {
        let decal = self
            .decals
            .iter()
            .find(|d| d.name == decal_name)
            .ok_or_else(|| format!("Decal '{}' not found", decal_name))?;

        for path in decal.fields.values() {
            let _ = fs::remove_file(path);
        }
        let _ = fs::remove_file(&decal.json_path);
        if let Some(parent) = decal.json_path.parent() {
            let _ = fs::remove_dir(parent);
        }

        self.active_decals.retain(|_, v| v != decal_name);
        config.patcher.active_decals = self.active_decals.clone();
        let _ = config.save(&self.base_dir);

        self.refresh_decals();
        let _ = tx.send(AppMsg::Log(format!("[Decals] Deleted '{}'", decal_name)));
        Ok(())
    }

    pub fn handle_op_result(&mut self, tx: &Sender<AppMsg>, config: &mut Config) {
        let mut active_changed = false;
        while let Ok(op) = self.local_rx.try_recv() {
            match op {
                DecalOp::Applied {
                    name,
                    active_key,
                    target_upk,
                } => {
                    self.processing_target = None;
                    self.progress = None;
                    self.progress_label.clear();
                    // The C# patcher keeps one custom decal active. TFC append
                    // storage is shared, so prior targets are restored before
                    // a new target is installed.
                    self.active_decals.clear();
                    self.active_decals.insert(active_key, name.clone());
                    active_changed = true;
                    let _ = tx.send(AppMsg::Log(format!(
                        "[Decals] Successfully applied '{}' to {}!",
                        name, target_upk
                    )));
                }
                DecalOp::Restored { active_key, name } => {
                    self.processing_target = None;
                    self.progress = None;
                    self.progress_label.clear();
                    self.active_decals.remove(&active_key);
                    active_changed = true;
                    let _ = tx.send(AppMsg::Log(format!(
                        "[Decals] Restored '{}' successfully!",
                        name
                    )));
                }
                DecalOp::RestoredAll { count } => {
                    self.processing_target = None;
                    self.progress = None;
                    self.progress_label.clear();
                    self.active_decals.clear();
                    active_changed = true;
                    let _ = tx.send(AppMsg::Log(format!("[Decals] Restored {} file(s)", count)));
                }
                DecalOp::Progress { fraction, label } => {
                    self.progress = Some(fraction.clamp(0.0, 1.0));
                    self.progress_label = label;
                }
                DecalOp::Error(e) => {
                    self.processing_target = None;
                    self.progress = None;
                    self.progress_label.clear();
                    let _ = tx.send(AppMsg::Log(format!("[Decals] Error: {}", e)));
                }
            }
        }
        if active_changed {
            config.patcher.active_decals = self.active_decals.clone();
            let _ = config.save(&self.base_dir);
        }
    }

    pub fn render_tab(
        &mut self,
        ui: &mut egui::Ui,
        cooked_pc: &Path,
        backups_dir: &Path,
        tx: &Sender<AppMsg>,
        ctx: &egui::Context,
        config: &mut Config,
    ) {
        // Handle async operations
        self.handle_op_result(tx, config);
        // Start below the app's global tab/header area. Using max_rect here
        // also covered the main navigation and made the transparent window
        // show the desktop through the entire app while a decal was running.
        let decal_tab_rect = ui.available_rect_before_wrap();

        ui.heading("Decal Patcher");
        ui.add_space(4.0);

        let key_ok = self.validate_key_file().is_ok();

        if let Some(ref error) = self.catalog_error {
            ui.colored_label(
                egui::Color32::from_rgb(0xe7, 0x4c, 0x3c),
                format!("⚠ {}", error),
            );
            ui.add_space(8.0);
            if ui.button("Retry Loading Catalog").clicked() {
                self.load_car_skins();
            }
            ui.add_space(8.0);
        }

        ui.horizontal(|ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add_enabled(
                        self.processing_target.is_none(),
                        egui::Button::new("Refresh"),
                    )
                    .clicked()
                {
                    self.refresh_decals();
                    self.load_car_skins();
                    let _ = tx.send(AppMsg::Log("[Decals] Decal list refreshed.".to_string()));
                }
                let has_active = !self.active_decals.is_empty();
                let is_processing = self.processing_target.is_some();
                if ui
                    .add_enabled(
                        has_active && !is_processing && key_ok,
                        egui::Button::new("Restore Original")
                            .fill(egui::Color32::from_rgb(180, 50, 50)),
                    )
                    .clicked()
                {
                    self.restore_all_confirmed = true;
                }

                if ui
                    .add_enabled(!is_processing, egui::Button::new("Import ZIP"))
                    .clicked()
                {
                    if let Some(file) = rfd::FileDialog::new()
                        .add_filter("ZIP Archives", &["zip"])
                        .pick_file()
                    {
                        if let Err(e) = self.import_zip(&file, tx) {
                            let _ = tx.send(AppMsg::Log(format!("[Decals] Import failed: {}", e)));
                        }
                    }
                }
                if ui
                    .checkbox(&mut self.show_applied, "Show Applied")
                    .changed()
                {
                    self.page = 0;
                }
            });
        });

        ui.add_space(8.0);
        ui.separator();
        ui.add_space(8.0);

        // Restore All confirmation
        if self.restore_all_confirmed {
            let mut close = false;
            egui::Window::new("Confirm Restore All")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.label("This will restore ALL decals to their original state.");
                    ui.label(format!(
                        "{} decal(s) will be reverted.",
                        self.active_decals.len()
                    ));
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("Restore All").clicked() {
                            if let Err(e) = self.restore_all_decals(cooked_pc, backups_dir, tx, ctx)
                            {
                                let _ =
                                    tx.send(AppMsg::Log(format!("[Decals] Restore failed: {}", e)));
                            }
                            close = true;
                        }
                        if ui.button("Cancel").clicked() {
                            close = true;
                        }
                    });
                });
            if close {
                self.restore_all_confirmed = false;
            }
        }

        ui.horizontal(|ui| {
            let row_width = ui.available_width();
            let left_width = (row_width * 0.48).max(280.0);
            let right_width = (row_width - left_width - 16.0).max(280.0);

            // Left panel: Available decals
            ui.allocate_ui_with_layout(
                egui::vec2(left_width, 440.0),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    ui.label(egui::RichText::new("Available Decals").strong());
                    ui.add_space(4.0);

                    ui.horizontal(|ui| {
                        ui.strong("Search:");
                        let search_resp = ui.add(
                            egui::TextEdit::singleline(&mut self.search_input)
                                .hint_text("Search decals...")
                                .desired_width(150.0),
                        );
                        let submitted = search_resp.lost_focus()
                            && ui.input(|i| i.key_pressed(egui::Key::Enter));
                        if ui.button("🔍").clicked() || submitted {
                            self.search_filter = self.search_input.clone();
                            self.page = 0;
                        }
                        if ui.button("Clear").clicked() {
                            self.search_input.clear();
                            self.search_filter.clear();
                            self.page = 0;
                        }
                    });
                    ui.add_space(4.0);

                    let decal_body_ids: HashSet<i32> =
                        self.decals.iter().map(|decal| decal.body_id).collect();
                    let decal_car_keys: HashMap<i32, String> = decal_body_ids
                        .into_iter()
                        .filter_map(|body_id| {
                            self.car_key_for_body_id(body_id)
                                .map(|car_key| (body_id, car_key))
                        })
                        .collect();
                    let query = self.search_filter.to_lowercase().trim().to_string();
                    let filtered: Vec<&DecalItem> = self
                        .decals
                        .iter()
                        .filter(|d| {
                            d.name.to_lowercase().contains(&query)
                                && (!self.show_applied
                                    || self.active_decals.values().any(|name| name == &d.name))
                        })
                        .collect();
                    const PAGE_SIZE: usize = 12;
                    let total_pages = filtered.len().div_ceil(PAGE_SIZE).max(1);
                    self.page = self.page.min(total_pages - 1);
                    ui.horizontal(|ui| {
                        ui.label(format!("Page {} of {}", self.page + 1, total_pages));
                        if ui
                            .add_enabled(self.page > 0, egui::Button::new("Previous"))
                            .clicked()
                        {
                            self.page -= 1;
                        }
                        if ui
                            .add_enabled(self.page + 1 < total_pages, egui::Button::new("Next"))
                            .clicked()
                        {
                            self.page += 1;
                        }
                    });
                    let visible = &filtered
                        [self.page * PAGE_SIZE..((self.page + 1) * PAGE_SIZE).min(filtered.len())];

                    egui::ScrollArea::vertical()
                        .id_salt("available_decals")
                        .auto_shrink([false, false])
                        .max_height(400.0)
                        .show(ui, |ui| {
                            if filtered.is_empty() {
                                ui.label(
                                    egui::RichText::new("No decals found")
                                        .color(egui::Color32::GRAY),
                                );
                                return;
                            }

                            for row in visible.chunks(2) {
                                ui.columns(2, |columns| {
                                    for (column, decal) in row.iter().enumerate() {
                                        let is_selected = self.selected_decal_name.as_deref()
                                            == Some(decal.name.as_str());
                                        egui::Frame::group(columns[column].style()).show(
                                            &mut columns[column],
                                            |ui| {
                                                ui.set_min_height(172.0);
                                                ui.vertical_centered(|ui| {
                                                    let size = egui::vec2(140.0, 96.0);
                                                    if let Some(bytes) = &decal.preview_image {
                                                        ui.add(
                                                            egui::Image::from_bytes(
                                                                format!(
                                                                    "bytes://decal/preview/{}",
                                                                    decal.name
                                                                ),
                                                                bytes.clone(),
                                                            )
                                                            .fit_to_exact_size(size),
                                                        );
                                                    } else {
                                                        ui.add_sized(
                                                            size,
                                                            egui::Label::new("No Image"),
                                                        );
                                                    }
                                                    if ui
                                                        .selectable_label(
                                                            is_selected,
                                                            egui::RichText::new(&decal.name)
                                                                .strong(),
                                                        )
                                                        .clicked()
                                                    {
                                                        self.selected_decal_name =
                                                            Some(decal.name.clone());
                                                        let next_car = decal_car_keys
                                                            .get(&decal.body_id)
                                                            .cloned();
                                                        if self.selected_car != next_car {
                                                            self.selected_car = next_car;
                                                            self.selected_skin_id = None;
                                                            self.skin_dropdown_filter.clear();
                                                        }
                                                    }
                                                    if self.processing_target.as_deref()
                                                        == Some(decal.name.as_str())
                                                    {
                                                        ui.spinner();
                                                    }
                                                    if ui
                                                        .add_enabled(
                                                            self.processing_target.is_none(),
                                                            egui::Button::new("Delete")
                                                                .fill(egui::Color32::from_rgb(
                                                                    180, 50, 50,
                                                                ))
                                                                .small(),
                                                        )
                                                        .clicked()
                                                    {
                                                        self.confirm_delete =
                                                            Some((*decal).clone());
                                                    }
                                                });
                                            },
                                        );
                                    }
                                });
                                ui.add_space(6.0);
                            }
                        });
                },
            );

            ui.separator();
            ui.add_space(8.0);

            // Right panel: Car/Skin selection
            ui.allocate_ui_with_layout(
                egui::vec2(right_width, 440.0),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    ui.label(egui::RichText::new("Target Skin").strong());
                    ui.add_space(4.0);

                    let selected_car_clone = self.selected_car.clone();
                    let selected_skin_id_clone = self.selected_skin_id.clone();
                    let selected_decal_name_clone = self.selected_decal_name.clone();
                    let target_has_active = selected_car_clone
                        .as_ref()
                        .zip(selected_skin_id_clone.as_ref())
                        .is_some_and(|(car, skin)| {
                            self.active_decals.contains_key(&format!("{car}|{skin}"))
                        });

                    if let Some(car) = self.selected_decal_car() {
                        ui.label(egui::RichText::new(format!("Car: {}", car.car_name)).strong());
                    } else if selected_decal_name_clone.is_some() {
                        ui.colored_label(
                            egui::Color32::from_rgb(230, 160, 60),
                            "This decal's BodyID does not match a car in the catalogs",
                        );
                    } else {
                        ui.label(
                            egui::RichText::new("Select an imported decal first")
                                .color(egui::Color32::GRAY),
                        );
                    }

                    // Applied targets are locked until restored, matching the actual file state.
                    ui.add_enabled_ui(!target_has_active, |ui| {
                        // SKIN DROPDOWN
                        ui.horizontal(|ui| {
                            ui.label("Decal to replace:");

                            let car_info = selected_car_clone
                                .as_ref()
                                .and_then(|car_key| {
                                    self.car_skins.iter().find(|c| &c.car_key == car_key)
                                })
                                .cloned();

                            let mut selected_skin_id =
                                selected_skin_id_clone.clone().unwrap_or_default();

                            if let Some(car) = car_info {
                                egui::ComboBox::from_id_salt("skin_combo")
                                    .width(250.0)
                                    .height(300.0)
                                    .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
                                    .selected_text(
                                        car.skins
                                            .iter()
                                            .find(|s| {
                                                Some(&s.id) == selected_skin_id_clone.as_ref()
                                            })
                                            .map(|s| s.name.clone())
                                            .unwrap_or_else(|| "Select decal...".to_string()),
                                    )
                                    .show_ui(ui, |ui| {
                                        ui.horizontal(|ui| {
                                            ui.label("Filter:");
                                            ui.add_sized(
                                                [185.0, 20.0],
                                                egui::TextEdit::singleline(
                                                    &mut self.skin_dropdown_filter,
                                                )
                                                .hint_text("Search decals..."),
                                            );
                                            if ui.small_button("×").clicked() {
                                                self.skin_dropdown_filter.clear();
                                            }
                                        });
                                        ui.separator();

                                        let query = self.skin_dropdown_filter.trim().to_lowercase();
                                        let mut sorted_skins: Vec<(String, String)> = car
                                            .skins
                                            .iter()
                                            .filter(|skin| {
                                                query.is_empty()
                                                    || skin.name.to_lowercase().contains(&query)
                                                    || skin.id.to_lowercase().contains(&query)
                                            })
                                            .map(|skin| (skin.id.clone(), skin.name.clone()))
                                            .collect();
                                        sorted_skins.sort_by(|a, b| a.1.cmp(&b.1));

                                        if sorted_skins.is_empty() {
                                            ui.label(
                                                egui::RichText::new("No decals match the filter")
                                                    .color(egui::Color32::GRAY),
                                            );
                                        } else {
                                            for (skin_id, skin_name) in sorted_skins {
                                                if ui
                                                    .selectable_value(
                                                        &mut selected_skin_id,
                                                        skin_id.clone(),
                                                        skin_name,
                                                    )
                                                    .changed()
                                                {
                                                    self.selected_skin_id = Some(skin_id);
                                                }
                                            }
                                        }
                                    });
                            } else if self.car_skins.is_empty() {
                                ui.label("No decals available - check skins.json");
                            } else {
                                ui.label("Select an imported decal first");
                            }
                        });
                    });

                    ui.add_space(8.0);

                    if let (Some(car_key), Some(skin_id)) =
                        (selected_car_clone.as_ref(), selected_skin_id_clone.as_ref())
                    {
                        let active_key = format!("{}|{}", car_key, skin_id);
                        if let Some(applied) = self.active_decals.get(&active_key) {
                            let car_name = self
                                .car_skins
                                .iter()
                                .find(|car| &car.car_key == car_key)
                                .map(|car| car.car_name.as_str())
                                .unwrap_or(car_key);
                            let skin_name = self
                                .car_skins
                                .iter()
                                .find(|car| &car.car_key == car_key)
                                .and_then(|car| car.skins.iter().find(|skin| &skin.id == skin_id))
                                .map(|skin| skin.name.as_str())
                                .unwrap_or(skin_id);
                            ui.colored_label(
                                egui::Color32::from_rgb(46, 204, 113),
                                format!("Set to {car_name}\n{skin_name}\nApplied decal: {applied}"),
                            );
                        } else {
                            ui.colored_label(egui::Color32::GRAY, "No decal applied to this skin");
                        }
                    }

                    ui.add_space(8.0);

                    let can_apply = selected_decal_name_clone.is_some()
                        && selected_car_clone.is_some()
                        && selected_skin_id_clone.is_some()
                        && key_ok
                        && self.processing_target.is_none();

                    let is_restoring = selected_car_clone.is_some()
                        && selected_skin_id_clone.is_some()
                        && self.active_decals.contains_key(&format!(
                            "{}|{}",
                            selected_car_clone.as_ref().unwrap(),
                            selected_skin_id_clone.as_ref().unwrap()
                        ))
                        && self.processing_target.is_none();

                    ui.horizontal(|ui| {
                        if is_restoring {
                            if ui
                                .add_enabled(
                                    key_ok && self.processing_target.is_none(),
                                    egui::Button::new("Restore")
                                        .fill(egui::Color32::from_rgb(200, 100, 50)),
                                )
                                .clicked()
                            {
                                if let (Some(car_key), Some(skin_id)) =
                                    (selected_car_clone.as_ref(), selected_skin_id_clone.as_ref())
                                {
                                    if let Err(e) = self.restore_decal_from_skin(
                                        car_key,
                                        skin_id,
                                        cooked_pc,
                                        backups_dir,
                                        tx,
                                        ctx,
                                    ) {
                                        let _ = tx.send(AppMsg::Log(format!(
                                            "[Decals] Restore failed: {}",
                                            e
                                        )));
                                    } else {
                                        config.patcher.active_decals = self.active_decals.clone();
                                        let _ = config.save(&self.base_dir);
                                    }
                                }
                            }
                        } else {
                            if ui
                                .add_enabled(
                                    can_apply,
                                    egui::Button::new("Apply Decal")
                                        .fill(egui::Color32::from_rgb(46, 204, 113)),
                                )
                                .clicked()
                            {
                                if let (Some(decal_name), Some(car_key), Some(skin_id)) = (
                                    selected_decal_name_clone.as_ref(),
                                    selected_car_clone.as_ref(),
                                    selected_skin_id_clone.as_ref(),
                                ) {
                                    if let Err(e) = self.apply_decal_to_skin(
                                        decal_name,
                                        car_key,
                                        skin_id,
                                        cooked_pc,
                                        backups_dir,
                                        tx,
                                        ctx,
                                    ) {
                                        let _ = tx.send(AppMsg::Log(format!(
                                            "[Decals] Apply failed: {}",
                                            e
                                        )));
                                    } else {
                                        config.patcher.active_decals = self.active_decals.clone();
                                        let _ = config.save(&self.base_dir);
                                    }
                                }
                            }
                        }
                    });

                    if self.selected_decal_name.is_none() && !self.decals.is_empty() {
                        ui.label(
                            egui::RichText::new("Select a decal from the left panel")
                                .color(egui::Color32::GRAY)
                                .size(11.0),
                        );
                    }
                },
            );
        });

        // Delete confirmation
        if let Some(decal_to_delete) = self.confirm_delete.clone() {
            let mut close = false;
            egui::Window::new("Confirm Deletion")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.label(format!(
                        "Are you sure you want to delete '{}'?",
                        decal_to_delete.name
                    ));
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("Yes").clicked() {
                            if let Err(e) = self.delete_decal(&decal_to_delete.name, tx, config) {
                                let _ =
                                    tx.send(AppMsg::Log(format!("[Decals] Delete failed: {}", e)));
                            }
                            close = true;
                        }
                        if ui.button("No").clicked() {
                            close = true;
                        }
                    });
                });
            if close {
                self.confirm_delete = None;
            }
        }

        if self.processing_target.is_some() {
            ui.painter().rect_filled(
                decal_tab_rect,
                0.0,
                egui::Color32::from_rgba_unmultiplied(20, 30, 40, 95),
            );
            ui.interact(
                decal_tab_rect,
                ui.id().with("decal_processing_overlay"),
                egui::Sense::click(),
            );
            ui.painter().text(
                decal_tab_rect.center(),
                egui::Align2::CENTER_CENTER,
                if self.progress_label.is_empty() {
                    "Working..."
                } else {
                    self.progress_label.as_str()
                },
                egui::FontId::proportional(16.0),
                egui::Color32::WHITE,
            );
        }
    }
}
