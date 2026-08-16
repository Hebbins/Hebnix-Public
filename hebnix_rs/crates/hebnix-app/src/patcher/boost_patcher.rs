use aes::Aes256;
use aes::cipher::generic_array::GenericArray;
use aes::cipher::{BlockDecrypt, BlockEncrypt, KeyInit};
#[cfg(test)]
use base64::Engine;
#[cfg(test)]
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use crossbeam_channel::{Receiver, Sender};
use eframe::egui;
use image::imageops::FilterType;
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::config::Config;
use crate::messages::AppMsg;
use crate::patch_core::{dxt, upk};

const GFX_UPK: &str = "GFX_Hud_SF.upk";
const MAX_LOGICAL_PACKAGE_SIZE: usize = 512 * 1024 * 1024;

// JSON field, UPK export name, width, height. DXT5 uses one byte per pixel.
const TEXTURE_EXPORTS: [(&str, &str, usize, usize); 4] = [
    ("Background", "BoostMeter_Background", 256, 256),
    ("Fill", "BoostMeter_Fill", 256, 256),
    (
        "FillTintablePortion",
        "BoostMeter_FillTintablePortion",
        256,
        256,
    ),
    ("Glow", "BoostMeter_Glow", 64, 64),
];

#[derive(Clone, Debug)]
struct PackageHeader {
    licensee_version: u16,
    total_header_size: usize,
    name_count: usize,
    name_offset: usize,
    export_count: usize,
    export_offset: usize,
    generation_padding_size: usize,
    compressed_chunk_info_offset: usize,
}

#[derive(Clone, Debug)]
struct ChunkInfo {
    uncompressed_offset: usize,
    uncompressed_size: usize,
    compressed_offset: usize,
    compressed_size: usize,
    physical_size: usize,
    block_size: usize,
    table_entry_offset: usize,
}

#[derive(Clone, Debug)]
struct ExportEntry {
    object_name_index: usize,
    serial_size: usize,
    serial_offset: usize,
}

struct EncryptedPackage {
    raw: Vec<u8>,
    header: PackageHeader,
    key: [u8; 32],
    key_line: usize,
    encrypted_header_size: usize,
    decrypted_header: Vec<u8>,
    chunks: Vec<ChunkInfo>,
    logical_data: Vec<u8>,
    names: Vec<String>,
    exports: Vec<ExportEntry>,
    modified_chunks: HashSet<usize>,
}

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

    fn u64(&mut self) -> Result<u64, String> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
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

fn read_u32_at(data: &[u8], offset: usize) -> Result<u32, String> {
    let bytes = data
        .get(offset..offset + 4)
        .ok_or_else(|| format!("UPK field at {offset} is out of bounds"))?;
    Ok(u32::from_le_bytes(bytes.try_into().unwrap()))
}

fn read_i32_at(data: &[u8], offset: usize) -> Result<i32, String> {
    Ok(read_u32_at(data, offset)? as i32)
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

fn read_package_header(raw: &[u8]) -> Result<PackageHeader, String> {
    let mut reader = ByteCursor::new(raw);
    if reader.u32()? != upk::UPK_MAGIC {
        return Err("GFX_Hud_SF has an invalid UPK signature".to_string());
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
    let _import_count = non_negative(reader.i32()?, "import count")?;
    let _import_offset = non_negative(reader.i32()?, "import offset")?;
    let _depends_offset = reader.i32()?;

    // Remaining plaintext FPackageFileSummary fields, matching the current
    // Rocket League UE3 package layout. The final two values locate the gap and
    // compressed-chunk table inside the encrypted name/header region.
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

fn load_upk_keys(path: &Path) -> Result<Vec<(usize, [u8; 32])>, String> {
    if path.as_os_str().is_empty() {
        return crate::upk_keys::embedded();
    }
    let text = fs::read_to_string(path).map_err(|error| {
        format!(
            "Failed to read test key catalog {}: {error}",
            path.display()
        )
    })?;
    crate::upk_keys::parse(&text, &path.display().to_string())
}

fn validate_name_prefix(decrypted: &[u8], name_count: usize) -> bool {
    let mut reader = ByteCursor::new(decrypted);
    for _ in 0..name_count.min(5) {
        let start = reader.pos;
        let Ok(len) = reader.i32() else {
            return false;
        };
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

fn non_negative_i64(value: i64, field: &str) -> Result<usize, String> {
    usize::try_from(value).map_err(|_| format!("Invalid negative or oversized {field}: {value}"))
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

    // Current Rocket League packages may include 12 reserved bytes after each
    // compressed-chunk row. Detect that layout from the second logical offset.
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
            || read_u32_at(raw, compressed_offset)? != upk::UPK_MAGIC
        {
            return Err(format!(
                "UPK chunk {index} points to invalid compressed data at {compressed_offset}"
            ));
        }
        if index > 0 {
            let previous: &ChunkInfo = chunks.last().unwrap();
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
            compressed_size,
            physical_size: 0,
            block_size: 0,
            table_entry_offset: entry,
        });
    }
    Ok(chunks)
}

fn find_package_key(
    raw: &[u8],
    header: &PackageHeader,
    key_file: &Path,
) -> Result<([u8; 32], usize, Vec<u8>, Vec<ChunkInfo>), String> {
    let size = encrypted_header_size(header)?;
    let encrypted = raw
        .get(header.name_offset..header.name_offset + size)
        .ok_or("File is too small for its encrypted UPK header")?;
    let keys = load_upk_keys(key_file)?;
    for (line, key) in &keys {
        let Ok(decrypted) = aes_decrypt_ecb(encrypted, key) else {
            continue;
        };
        if !validate_name_prefix(&decrypted, header.name_count) {
            continue;
        }
        let Ok(chunks) = parse_chunk_table(&decrypted, header, raw) else {
            continue;
        };
        return Ok((*key, *line, decrypted, chunks));
    }
    Err(format!(
        "None of the {} available keys can decrypt this GFX_Hud_SF package",
        keys.len()
    ))
}

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
        reader.i32()?;
        reader.i32()?;
        reader.i32()?;
        let object_name_index = non_negative(reader.i32()?, "export name index")?;
        reader.i32()?;
        reader.i32()?;
        reader.u64()?;
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
            object_name_index,
            serial_size,
            serial_offset,
        });
    }
    Ok(exports)
}

impl EncryptedPackage {
    fn load(raw: Vec<u8>, key_file: &Path) -> Result<Self, String> {
        let header = read_package_header(&raw)?;
        let encrypted_header_size = encrypted_header_size(&header)?;
        let (key, key_line, decrypted_header, mut chunks) =
            find_package_key(&raw, &header, key_file)?;

        let mut decompressed_chunks = Vec::with_capacity(chunks.len());
        let mut logical_size = raw.len();
        for (index, chunk) in chunks.iter_mut().enumerate() {
            let (decompressed, block_size, physical_end) =
                upk::decomp_chunk_at(&raw, chunk.compressed_offset).map_err(|error| {
                    format!("Failed to decompress GFX_Hud_SF chunk {index}: {error:?}")
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
                "GFX_Hud_SF expands to {logical_size} bytes, above the safety limit"
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
            return Err("GFX_Hud_SF export table contains an invalid name index".to_string());
        }

        Ok(Self {
            raw,
            header,
            key,
            key_line,
            encrypted_header_size,
            decrypted_header,
            chunks,
            logical_data,
            names,
            exports,
            modified_chunks: HashSet::new(),
        })
    }

    fn texture_range(
        &self,
        export_name: &str,
        width: usize,
        height: usize,
    ) -> Result<(usize, usize), String> {
        let export = self
            .exports
            .iter()
            .find(|export| self.names[export.object_name_index].eq_ignore_ascii_case(export_name))
            .ok_or_else(|| format!("Texture export {export_name} was not found in GFX_Hud_SF"))?;
        let dxt_size = width
            .checked_mul(height)
            .ok_or("Texture dimensions are too large")?;
        let prefix_size = export
            .serial_size
            .checked_sub(dxt_size + 60)
            .ok_or_else(|| format!("Texture export {export_name} is smaller than expected"))?;
        if prefix_size > 4096 {
            return Err(format!(
                "Texture export {export_name} has an unexpected {prefix_size}-byte prefix"
            ));
        }
        let start = export
            .serial_offset
            .checked_add(prefix_size)
            .ok_or("Texture export offset overflow")?;
        let end = start.checked_add(dxt_size).ok_or("Texture size overflow")?;
        if end
            .checked_add(60)
            .is_none_or(|tail| tail > self.logical_data.len())
        {
            return Err(format!(
                "Texture export {export_name} is outside GFX_Hud_SF"
            ));
        }
        Ok((start, end))
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
            .ok_or("Boost texture data is not contained in a compressed UPK chunk")?;
        self.modified_chunks.insert(index);
        Ok(())
    }
}

fn compress_into_chunk(payload: &[u8], block_size: usize) -> Result<Vec<u8>, String> {
    if payload.is_empty() {
        return Err("Cannot compress an empty GFX_Hud_SF chunk".to_string());
    }

    let block_size = if block_size == 0 { 131_072 } else { block_size };

    let mut blocks = Vec::new();
    for block in payload.chunks(block_size) {
        blocks.push(
            upk::zlib_compress(block, 9)
                .map_err(|e| format!("Failed to compress GFX_Hud_SF chunk: {e:?}"))?,
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

    upk::write_u32(&mut output, 0, upk::UPK_MAGIC);
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

    let encrypted_header = aes_encrypt_ecb(&decrypted_header, &package.key)?;
    if encrypted_header.len() != package.encrypted_header_size {
        return Err("Re-encrypted UPK header size changed; refusing to write".to_string());
    }
    let encrypted_end = package.header.name_offset + package.encrypted_header_size;
    if encrypted_end > output.len() {
        return Err("Re-encrypted UPK header no longer fits the package".to_string());
    }
    output[package.header.name_offset..encrypted_end].copy_from_slice(&encrypted_header);

    validate_repacked_package(package, &output)?;
    Ok(output)
}

fn validate_repacked_package(package: &EncryptedPackage, output: &[u8]) -> Result<(), String> {
    let encrypted_end = package.header.name_offset + package.encrypted_header_size;
    let decrypted = aes_decrypt_ecb(
        output
            .get(package.header.name_offset..encrypted_end)
            .ok_or("Generated UPK is missing its encrypted header")?,
        &package.key,
    )?;
    let chunks = parse_chunk_table(&decrypted, &package.header, output)
        .map_err(|error| format!("Generated UPK header failed validation: {error}"))?;
    if chunks.len() != package.chunks.len() {
        return Err("Generated UPK chunk count changed unexpectedly".to_string());
    }
    for index in &package.modified_chunks {
        let chunk = &chunks[*index];
        let expected_chunk = &package.chunks[*index];
        let expected_len = package
            .chunks
            .get(index + 1)
            .and_then(|next| {
                next.uncompressed_offset
                    .checked_sub(expected_chunk.uncompressed_offset)
            })
            .unwrap_or(expected_chunk.uncompressed_size)
            .max(expected_chunk.uncompressed_size);
        let expected_end = expected_chunk.uncompressed_offset + expected_len;
        let expected = &package.logical_data[expected_chunk.uncompressed_offset..expected_end];
        let (actual, _, physical_end) = upk::decomp_chunk_at(output, chunk.compressed_offset)
            .map_err(|error| format!("Generated UPK chunk {index} failed validation: {error:?}"))?;
        if actual != expected {
            return Err(format!(
                "Generated UPK chunk {index} did not round-trip exactly"
            ));
        }
        let physical_size = physical_end - chunk.compressed_offset;
        if physical_size != chunk.compressed_size {
            return Err(format!(
                "Generated UPK chunk {index} size does not match its encrypted table"
            ));
        }
    }
    Ok(())
}

fn dxt5_alpha_block(a: &[u8; 16]) -> [u8; 8] {
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

    let mut out = [0u8; 8];
    out[0] = a_max;
    out[1] = a_min;

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
        out[2 + i] = ((indices >> (i * 8)) & 0xFF) as u8;
    }
    out
}

fn resize_rgba_isolated(img: &image::RgbaImage, dst_w: u32, dst_h: u32) -> image::RgbaImage {
    let (src_w, src_h) = img.dimensions();
    let mut r = image::GrayImage::new(src_w, src_h);
    let mut g = image::GrayImage::new(src_w, src_h);
    let mut b = image::GrayImage::new(src_w, src_h);
    let mut a = image::GrayImage::new(src_w, src_h);

    for (x, y, px) in img.enumerate_pixels() {
        r.put_pixel(x, y, image::Luma([px[0]]));
        g.put_pixel(x, y, image::Luma([px[1]]));
        b.put_pixel(x, y, image::Luma([px[2]]));
        a.put_pixel(x, y, image::Luma([px[3]]));
    }

    let r = image::imageops::resize(&r, dst_w, dst_h, FilterType::CatmullRom);
    let g = image::imageops::resize(&g, dst_w, dst_h, FilterType::CatmullRom);
    let b = image::imageops::resize(&b, dst_w, dst_h, FilterType::CatmullRom);
    let a = image::imageops::resize(&a, dst_w, dst_h, FilterType::CatmullRom);

    let mut out = image::RgbaImage::new(dst_w, dst_h);
    for y in 0..dst_h {
        for x in 0..dst_w {
            out.put_pixel(
                x,
                y,
                image::Rgba([
                    r.get_pixel(x, y)[0],
                    g.get_pixel(x, y)[0],
                    b.get_pixel(x, y)[0],
                    a.get_pixel(x, y)[0],
                ]),
            );
        }
    }
    out
}

pub fn image_to_dxt5_with_alpha(img: &image::RgbaImage, width: usize, height: usize) -> Vec<u8> {
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
                    a[idx] = pixel[3];
                }
            }

            let alpha_block = dxt5_alpha_block(&a);
            dxt5[out_idx..out_idx + 8].copy_from_slice(&alpha_block);

            let mut max_rgb565: i32 = -1;
            let mut min_rgb565: i32 = -1;

            for i in 0..16 {
                let c_rgb565 = dxt::rgb_to_rgb565(r[i], g[i], b[i]) as i32;
                if max_rgb565 < 0 || c_rgb565 > max_rgb565 {
                    max_rgb565 = c_rgb565;
                }
                if min_rgb565 < 0 || c_rgb565 < min_rgb565 {
                    min_rgb565 = c_rgb565;
                }
            }

            if max_rgb565 < min_rgb565 {
                std::mem::swap(&mut max_rgb565, &mut min_rgb565);
            }

            let mut rgb565_0 = max_rgb565 as u16;
            let rgb565_1 = min_rgb565 as u16;
            let mut color_indices = 0u32;

            if rgb565_0 == rgb565_1 {
                rgb565_0 = rgb565_0.saturating_add(1);
                color_indices = 1431655765;
            } else {
                let (r0, g0, b0) = dxt::rgb565_to_rgb(rgb565_0);
                let (r1, g1, b1) = dxt::rgb565_to_rgb(rgb565_1);

                let palettes = [
                    [r0, g0, b0],
                    [r1, g1, b1],
                    [(2 * r0 + r1) / 3, (2 * g0 + g1) / 3, (2 * b0 + b1) / 3],
                    [(r0 + 2 * r1) / 3, (g0 + 2 * g1) / 3, (b0 + 2 * b1) / 3],
                ];

                for i in 0..16 {
                    let mut best_dist = i32::MAX;
                    let mut best_idx = 0;
                    for j in 0..4 {
                        let dr = r[i] - palettes[j][0];
                        let dg = g[i] - palettes[j][1];
                        let db = b[i] - palettes[j][2];
                        let dist = dr * dr + dg * dg + db * db;
                        if dist < best_dist {
                            best_dist = dist;
                            best_idx = j;
                        }
                    }
                    color_indices |= (best_idx as u32) << (i * 2);
                }
            }

            dxt5[out_idx + 8..out_idx + 10].copy_from_slice(&rgb565_0.to_le_bytes());
            dxt5[out_idx + 10..out_idx + 12].copy_from_slice(&rgb565_1.to_le_bytes());
            dxt5[out_idx + 12..out_idx + 16].copy_from_slice(&color_indices.to_le_bytes());

            out_idx += 16;
        }
    }
    dxt5
}

pub fn patch_boost_meter(
    game_dir: &str,
    backup_dir: &str,
    key_file: &Path,
    png_data_map: &HashMap<String, Vec<u8>>,
) -> Result<(), String> {
    let upk_path = Path::new(game_dir).join(GFX_UPK);
    if !upk_path.exists() {
        return Err(format!("{} not found in game directory.", GFX_UPK));
    }

    // Validate the key catalog before changing backups or the live game.
    load_upk_keys(key_file)?;

    fs::create_dir_all(backup_dir)
        .map_err(|e| format!("Failed to create backup directory: {e}"))?;
    let backup_upk = Path::new(backup_dir).join(format!("{}.bak", GFX_UPK));
    if !backup_upk.exists() {
        fs::copy(&upk_path, &backup_upk)
            .map_err(|e| format!("Failed to backup {}: {}", GFX_UPK, e))?;
    }

    let vanilla_raw = fs::read(&backup_upk).map_err(|e| format!("Failed to read backup: {}", e))?;
    let mut package = EncryptedPackage::load(vanilla_raw, key_file).map_err(|error| {
        format!(
            "Could not open the pristine GFX_Hud_SF backup: {error}. If this backup was made from a broken patch, restore/verify Rocket League and remove {} before trying again",
            backup_upk.display()
        )
    })?;
    tracing::info!(
        key_line = package.key_line,
        "matched GFX_Hud_SF encryption key from embedded catalog"
    );
    let mut patched_texture_count = 0usize;

    for (json_key, export_name, width, height) in TEXTURE_EXPORTS {
        if let Some(png_bytes) = png_data_map.get(json_key) {
            let img = image::load_from_memory(png_bytes)
                .map_err(|e| format!("Failed to load {}: {}", json_key, e))?
                .to_rgba8();
            let resized = resize_rgba_isolated(&img, width as u32, height as u32);
            let dxt5_data = image_to_dxt5_with_alpha(&resized, width, height);
            let (start, end) = package.texture_range(export_name, width, height)?;
            if dxt5_data.len() != end - start {
                return Err(format!(
                    "{}: DXT5 encoded size {} != expected {}",
                    json_key,
                    dxt5_data.len(),
                    end - start
                ));
            }
            package.logical_data[start..end].copy_from_slice(&dxt5_data);
            package.mark_modified_range(start, end)?;
            patched_texture_count += 1;
        }
    }

    if patched_texture_count == 0 {
        return Err("The selected boost pack contains no recognized texture images".to_string());
    }

    let final_out = repack_package(&package)?;
    fs::write(&upk_path, &final_out).map_err(|e| format!("Failed to write patched UPK: {}", e))?;
    Ok(())
}

#[derive(Deserialize)]
struct BoostDef {
    #[serde(rename = "Background")]
    background: Option<String>,
    #[serde(rename = "Fill")]
    fill: Option<String>,
    #[serde(rename = "Tint")]
    tint: Option<String>,
    #[serde(rename = "FillTintablePortion")]
    fill_tintable_portion: Option<String>,
    #[serde(rename = "Glow")]
    glow: Option<String>,
}

#[derive(Clone)]
pub struct BoostItem {
    pub name: String,
    pub json_path: PathBuf,
    pub image_paths: HashMap<String, PathBuf>,
    pub background_image: Option<Arc<[u8]>>,
    pub fill_image: Option<Arc<[u8]>>,
}

enum BoostOp {
    Applied(String),
    Restored,
    Error(String),
}

pub struct BoostPatcherState {
    base_dir: PathBuf,
    pub boosts_dir: PathBuf,
    pub boosts: Vec<BoostItem>,
    pub active_boost: Option<String>,
    pub processing_target: Option<String>,
    pub search_input: String,
    pub search_filter: String,
    pub show_applied: bool,
    pub page: usize,
    local_tx: Sender<BoostOp>,
    local_rx: Receiver<BoostOp>,
    pub confirm_delete: Option<BoostItem>,
}

impl BoostPatcherState {
    pub fn new(base_dir: &Path, config: &Config) -> Self {
        let boosts_dir = base_dir.join("boosts");
        let _ = fs::create_dir_all(&boosts_dir);

        let (local_tx, local_rx) = crossbeam_channel::unbounded();

        let mut state = Self {
            base_dir: base_dir.to_path_buf(),
            boosts_dir,
            boosts: Vec::new(),
            active_boost: config.patcher.active_boost.clone(),
            processing_target: None,
            search_input: String::new(),
            search_filter: String::new(),
            show_applied: false,
            page: 0,
            confirm_delete: None,
            local_tx,
            local_rx,
        };
        state.refresh_boosts();
        state
    }

    pub fn refresh_boosts(&mut self) {
        self.boosts.clear();
        if !self.boosts_dir.exists() {
            return;
        }

        let mut to_visit = vec![self.boosts_dir.clone()];
        while let Some(dir) = to_visit.pop() {
            if let Ok(entries) = fs::read_dir(&dir) {
                for entry in entries.filter_map(|e| e.ok()) {
                    let path = entry.path();
                    if path.is_dir() {
                        to_visit.push(path);
                    } else if path.extension().and_then(|s| s.to_str()) == Some("json") {
                        self.parse_boost_json(&path);
                    }
                }
            }
        }
    }

    fn parse_boost_json(&mut self, json_path: &Path) {
        if let Ok(content) = fs::read_to_string(json_path) {
            if let Ok(parsed) = serde_json::from_str::<HashMap<String, BoostDef>>(&content) {
                for (name, def) in parsed {
                    let mut image_paths = HashMap::new();
                    let mut background_image = None;
                    let mut fill_image = None;

                    if let Some(parent) = json_path.parent() {
                        let mut check_and_add = |key: &str, file_name: &Option<String>| {
                            if let Some(fname) = file_name {
                                let p = parent.join(fname);
                                if p.exists() {
                                    image_paths.insert(key.to_string(), p.clone());
                                    if key == "Background" {
                                        background_image = fs::read(&p).ok().map(Arc::from);
                                    } else if key == "Fill" {
                                        fill_image = fs::read(&p).ok().map(Arc::from);
                                    }
                                }
                            }
                        };

                        check_and_add("Background", &def.background);
                        check_and_add("Fill", &def.fill);
                        check_and_add(
                            "FillTintablePortion",
                            &def.tint.clone().or(def.fill_tintable_portion.clone()),
                        );
                        check_and_add("Glow", &def.glow);
                    }

                    if image_paths.contains_key("Background") && image_paths.contains_key("Fill") {
                        self.boosts.push(BoostItem {
                            name,
                            json_path: json_path.to_path_buf(),
                            image_paths,
                            background_image,
                            fill_image,
                        });
                    }
                }
            }
        }
    }

    fn import_zip(&mut self, zip_path: &Path, tx: &Sender<AppMsg>) {
        let _ = tx.send(AppMsg::Log("[Boost] Extracting ZIP...".to_string()));

        match (|| -> Result<(), String> {
            let file = fs::File::open(zip_path).map_err(|e| e.to_string())?;
            let mut archive = zip::ZipArchive::new(file).map_err(|e| e.to_string())?;

            let zip_stem = zip_path.file_stem().unwrap().to_string_lossy().to_string();
            let temp_dir = self.boosts_dir.join(format!("temp_{}", zip_stem));

            fs::create_dir_all(&temp_dir).map_err(|e| e.to_string())?;
            archive.extract(&temp_dir).map_err(|e| e.to_string())?;

            let entries: Vec<_> = fs::read_dir(&temp_dir)
                .unwrap()
                .filter_map(|e| e.ok())
                .collect();
            if entries.len() == 1 && entries[0].file_type().unwrap().is_dir() {
                let inner_dir = entries[0].path();
                let dest = self.boosts_dir.join(inner_dir.file_name().unwrap());
                let _ = fs::rename(&inner_dir, &dest);
                let _ = fs::remove_dir_all(&temp_dir);
            } else {
                let dest = self.boosts_dir.join(&zip_stem);
                let _ = fs::rename(&temp_dir, &dest);
            }
            Ok(())
        })() {
            Ok(_) => {
                let _ = tx.send(AppMsg::Log("[Boost] Imported successfully!".to_string()));
                self.refresh_boosts();
            }
            Err(e) => {
                let _ = tx.send(AppMsg::Log(format!("[Boost] Failed to import ZIP: {}", e)));
            }
        }
    }

    fn spawn_restore_thread(
        &mut self,
        cooked_pc: &Path,
        backups_dir: &Path,
        tx: &Sender<AppMsg>,
        ctx: &egui::Context,
    ) {
        self.processing_target = Some("Global_Restore".to_string());

        let cooked_pc_clone = cooked_pc.to_path_buf();
        let backups_dir_clone = backups_dir.to_path_buf();
        let local_tx = self.local_tx.clone();
        let ctx_clone = ctx.clone();

        let _ = tx.send(AppMsg::Log(
            "[Boost] Restoring original files...".to_string(),
        ));

        std::thread::spawn(move || {
            let backup_gfx = backups_dir_clone.join(format!("{}.bak", GFX_UPK));
            if backup_gfx.exists() && fs::copy(&backup_gfx, &cooked_pc_clone.join(GFX_UPK)).is_ok()
            {
                let _ = local_tx.send(BoostOp::Restored);
            } else {
                let _ = local_tx.send(BoostOp::Error("No backup found to restore.".into()));
            }
            ctx_clone.request_repaint();
        });
    }

    pub fn begin_restore(
        &mut self,
        cooked_pc: &Path,
        backups_dir: &Path,
        tx: &Sender<AppMsg>,
        ctx: &egui::Context,
    ) {
        self.spawn_restore_thread(cooked_pc, backups_dir, tx, ctx);
    }

    pub fn poll_ops(&mut self, tx: &Sender<AppMsg>, config: &mut Config) {
        while let Ok(op) = self.local_rx.try_recv() {
            self.processing_target = None;
            match op {
                BoostOp::Applied(name) => {
                    self.active_boost = Some(name.clone());
                    config.patcher.active_boost = Some(name.clone());
                    let _ = tx.send(AppMsg::Log(format!("[Boost] Successfully applied {name}!")));
                }
                BoostOp::Restored => {
                    self.active_boost = None;
                    config.patcher.active_boost = None;
                    let _ = tx.send(AppMsg::Log(
                        "[Boost] Restored original boost successfully.".into(),
                    ));
                }
                BoostOp::Error(error) => {
                    let _ = tx.send(AppMsg::Log(format!("[Boost] Error: {error}")));
                }
            }
            let _ = config.save(&self.base_dir);
        }
    }

    fn spawn_apply_thread(
        &mut self,
        boost: &BoostItem,
        cooked_pc: &Path,
        backups_dir: &Path,
        tx: &Sender<AppMsg>,
        ctx: &egui::Context,
    ) {
        self.processing_target = Some(boost.name.clone());
        let boost_name = boost.name.clone();
        let cooked_clone = cooked_pc.to_path_buf();
        let backups_clone = backups_dir.to_path_buf();
        let key_file_clone = PathBuf::new();
        let paths_clone = boost.image_paths.clone();
        let local_tx = self.local_tx.clone();
        let ctx_clone = ctx.clone();
        let _ = tx.send(AppMsg::Log(format!("[Boost] Patching {}...", boost.name)));
        std::thread::spawn(move || {
            let result = (|| -> Result<(), String> {
                let mut png_data = HashMap::new();
                for (slot, path) in &paths_clone {
                    png_data.insert(
                        slot.clone(),
                        fs::read(path)
                            .map_err(|error| format!("Failed to read {slot}: {error}"))?,
                    );
                }
                patch_boost_meter(
                    &cooked_clone.to_string_lossy(),
                    &backups_clone.to_string_lossy(),
                    &key_file_clone,
                    &png_data,
                )
            })();
            let _ = local_tx.send(match result {
                Ok(()) => BoostOp::Applied(boost_name),
                Err(error) => BoostOp::Error(error),
            });
            ctx_clone.request_repaint();
        });
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
        while let Ok(op) = self.local_rx.try_recv() {
            self.processing_target = None;
            match op {
                BoostOp::Applied(name) => {
                    self.active_boost = Some(name.clone());
                    config.patcher.active_boost = self.active_boost.clone();
                    let _ = config.save(&self.base_dir);
                    let _ = tx.send(AppMsg::Log(format!(
                        "[Boost] Successfully applied {}!",
                        name
                    )));
                }
                BoostOp::Restored => {
                    self.active_boost = None;
                    config.patcher.active_boost = None;
                    let _ = config.save(&self.base_dir);
                    let _ = tx.send(AppMsg::Log(
                        "[Boost] Restored original boost successfully.".to_string(),
                    ));
                }
                BoostOp::Error(e) => {
                    let _ = tx.send(AppMsg::Log(format!("[Boost] Error: {}", e)));
                }
            }
        }

        ui.heading("Boost Meter Patcher");
        ui.add_space(8.0);

        ui.horizontal(|ui| {
            ui.strong("Search:");
            let search_resp = ui.add(
                egui::TextEdit::singleline(&mut self.search_input)
                    .hint_text("Name or author...")
                    .desired_width(180.0),
            );
            let submitted =
                search_resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if ui.button("Search").clicked() || submitted {
                self.search_filter = self.search_input.clone();
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Refresh").clicked() {
                    self.refresh_boosts();
                    let _ = tx.send(AppMsg::Log(
                        "[Boost] Boost meter list refreshed.".to_string(),
                    ));
                }

                let restore_enabled =
                    self.active_boost.is_some() && self.processing_target.is_none();
                if ui
                    .add_enabled(
                        restore_enabled,
                        egui::Button::new("Restore Original")
                            .fill(egui::Color32::from_rgb(180, 50, 50)),
                    )
                    .clicked()
                {
                    self.spawn_restore_thread(cooked_pc, backups_dir, tx, ctx);
                }

                if ui
                    .add_enabled(
                        self.processing_target.is_none(),
                        egui::Button::new("Import ZIP"),
                    )
                    .clicked()
                {
                    if let Some(file) = rfd::FileDialog::new()
                        .add_filter("ZIP Archives", &["zip"])
                        .pick_file()
                    {
                        self.import_zip(&file, tx);
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

        egui::ScrollArea::vertical()
            .id_salt("patcher_boosts_scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let query = self.search_filter.to_lowercase().trim().to_string();
                let filtered: Vec<BoostItem> = self
                    .boosts
                    .iter()
                    .filter(|b| {
                        b.name.to_lowercase().contains(&query)
                            && (!self.show_applied
                                || self.active_boost.as_deref() == Some(b.name.as_str()))
                    })
                    .cloned()
                    .collect();

                if filtered.is_empty() {
                    ui.add_space(20.0);
                    ui.vertical_centered(|ui| {
                        if self.boosts.is_empty() {
                            ui.label(
                                egui::RichText::new(
                                    "No boost meters found in the /boosts/ directory.",
                                )
                                .color(egui::Color32::GRAY),
                            );
                        } else {
                            ui.label(
                                egui::RichText::new("No boost meters match your search.")
                                    .color(egui::Color32::GRAY),
                            );
                        }
                    });
                    return;
                }

                const PAGE_SIZE: usize = 20;
                let pages = filtered.len().div_ceil(PAGE_SIZE).max(1);
                self.page = self.page.min(pages - 1);
                ui.horizontal(|ui| {
                    ui.label(format!("Page {} of {}", self.page + 1, pages));
                    if ui
                        .add_enabled(self.page > 0, egui::Button::new("Previous"))
                        .clicked()
                    {
                        self.page -= 1;
                    }
                    if ui
                        .add_enabled(self.page + 1 < pages, egui::Button::new("Next"))
                        .clicked()
                    {
                        self.page += 1;
                    }
                });
                let start = self.page * PAGE_SIZE;
                let visible: Vec<_> = filtered.into_iter().skip(start).take(PAGE_SIZE).collect();
                for row in visible.chunks(5) {
                    ui.columns(5, |columns| {
                        for (column, boost) in row.iter().enumerate() {
                            egui::Frame::group(columns[column].style()).show(
                                &mut columns[column],
                                |ui| {
                                    ui.set_min_height(190.0);
                                    ui.vertical_centered(|ui| {
                                        let size = egui::vec2(120.0, 90.0);
                                        let (rect, _) =
                                            ui.allocate_exact_size(size, egui::Sense::hover());
                                        if let Some(bytes) = &boost.background_image {
                                            ui.put(
                                                rect,
                                                egui::Image::from_bytes(
                                                    format!(
                                                        "bytes://boost/background/{}",
                                                        boost.name
                                                    ),
                                                    bytes.clone(),
                                                )
                                                .fit_to_exact_size(size),
                                            );
                                        }
                                        if let Some(bytes) = &boost.fill_image {
                                            ui.put(
                                                rect,
                                                egui::Image::from_bytes(
                                                    format!("bytes://boost/fill/{}", boost.name),
                                                    bytes.clone(),
                                                )
                                                .fit_to_exact_size(size),
                                            );
                                        }
                                        if boost.background_image.is_none()
                                            && boost.fill_image.is_none()
                                        {
                                            ui.put(rect, egui::Label::new("No Image"));
                                        }
                                        ui.strong(&boost.name);
                                        ui.add_space(5.0);
                                        let busy = self.processing_target.is_some();
                                        if self.processing_target.as_deref()
                                            == Some(boost.name.as_str())
                                        {
                                            ui.spinner();
                                        } else if self.active_boost.as_deref()
                                            == Some(boost.name.as_str())
                                        {
                                            if ui
                                                .add_enabled(
                                                    !busy,
                                                    egui::Button::new("Restore").min_size(
                                                        egui::vec2(ui.available_width(), 24.0),
                                                    ),
                                                )
                                                .clicked()
                                            {
                                                self.spawn_restore_thread(
                                                    cooked_pc,
                                                    backups_dir,
                                                    tx,
                                                    ctx,
                                                );
                                            }
                                        } else if ui
                                            .add_enabled(
                                                !busy,
                                                egui::Button::new("Apply").min_size(egui::vec2(
                                                    ui.available_width(),
                                                    24.0,
                                                )),
                                            )
                                            .clicked()
                                        {
                                            self.spawn_apply_thread(
                                                boost,
                                                cooked_pc,
                                                backups_dir,
                                                tx,
                                                ctx,
                                            );
                                        }
                                        if ui
                                            .add_enabled(
                                                !busy,
                                                egui::Button::new("Delete")
                                                    .fill(egui::Color32::from_rgb(180, 50, 50))
                                                    .min_size(egui::vec2(
                                                        ui.available_width(),
                                                        24.0,
                                                    )),
                                            )
                                            .clicked()
                                        {
                                            self.confirm_delete = Some(boost.clone());
                                        }
                                    });
                                },
                            );
                        }
                    });
                    ui.add_space(6.0);
                }
            });

        if let Some(boost_to_delete) = self.confirm_delete.clone() {
            let mut close = false;
            egui::Window::new("Confirm Deletion")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.label(format!(
                        "Are you sure you want to delete '{}'?",
                        boost_to_delete.name
                    ));
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("Yes").clicked() {
                            for path in boost_to_delete.image_paths.values() {
                                let _ = fs::remove_file(path);
                            }
                            let _ = fs::remove_file(&boost_to_delete.json_path);

                            if let Some(parent) = boost_to_delete.json_path.parent() {
                                let _ = fs::remove_dir(parent);
                            }

                            if self.active_boost.as_deref() == Some(boost_to_delete.name.as_str()) {
                                self.active_boost = None;
                                config.patcher.active_boost = None;
                                let _ = config.save(&self.base_dir);
                            }

                            self.refresh_boosts();
                            let _ = tx.send(AppMsg::Log(format!(
                                "[Boost] Deleted boost '{}'",
                                boost_to_delete.name
                            )));
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use std::time::{SystemTime, UNIX_EPOCH};

    const TEST_KEY: [u8; 32] = [0x7a; 32];
    const WRONG_KEY: [u8; 32] = [0x31; 32];

    struct Fixture {
        raw: Vec<u8>,
    }

    fn push_u16(data: &mut Vec<u8>, value: u16) {
        data.extend_from_slice(&value.to_le_bytes());
    }

    fn push_u32(data: &mut Vec<u8>, value: u32) {
        data.extend_from_slice(&value.to_le_bytes());
    }

    fn push_i32(data: &mut Vec<u8>, value: i32) {
        data.extend_from_slice(&value.to_le_bytes());
    }

    fn push_u64(data: &mut Vec<u8>, value: u64) {
        data.extend_from_slice(&value.to_le_bytes());
    }

    fn push_i64(data: &mut Vec<u8>, value: i64) {
        data.extend_from_slice(&value.to_le_bytes());
    }

    fn set_i32(data: &mut [u8], offset: usize, value: i32) {
        data[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn set_i64(data: &mut [u8], offset: usize, value: i64) {
        data[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
    }

    fn push_fstring(data: &mut Vec<u8>, value: &str) {
        push_i32(data, i32::try_from(value.len() + 1).unwrap());
        data.extend_from_slice(value.as_bytes());
        data.push(0);
    }

    fn push_name(data: &mut Vec<u8>, value: &str) {
        push_fstring(data, value);
        data.extend_from_slice(&[0u8; 8]);
    }

    fn build_encrypted_fixture() -> Fixture {
        let mut summary = Vec::new();
        push_u32(&mut summary, upk::UPK_MAGIC);
        push_u16(&mut summary, 868);
        push_u16(&mut summary, 22);
        let total_header_size_pos = summary.len();
        push_i32(&mut summary, 0);
        push_fstring(&mut summary, "None");
        push_u32(&mut summary, 0);
        push_i32(&mut summary, TEXTURE_EXPORTS.len() as i32);
        let name_offset_pos = summary.len();
        push_i32(&mut summary, 0);
        push_i32(&mut summary, TEXTURE_EXPORTS.len() as i32);
        let export_offset_pos = summary.len();
        push_i32(&mut summary, 0);
        push_i32(&mut summary, 0); // import count
        push_i32(&mut summary, 0); // import offset
        push_i32(&mut summary, 0); // depends offset
        for _ in 0..4 {
            push_i32(&mut summary, 0);
        }
        summary.extend_from_slice(&[0u8; 16]);
        push_i32(&mut summary, 0); // generations
        push_u32(&mut summary, 0);
        push_u32(&mut summary, 0);
        push_u32(&mut summary, 0);
        push_i32(&mut summary, 0); // 16-byte records
        push_i32(&mut summary, 0);
        push_i32(&mut summary, 0); // additional strings
        push_i32(&mut summary, 0); // additional entries
        push_i32(&mut summary, 0); // generation padding size
        let chunk_info_offset_pos = summary.len();
        push_i32(&mut summary, 0);
        push_i32(&mut summary, 0);

        let name_offset = summary.len();
        let mut decrypted = Vec::new();
        for (_, export_name, _, _) in TEXTURE_EXPORTS {
            push_name(&mut decrypted, export_name);
        }
        let export_offset = name_offset + decrypted.len();

        let mut serial_offset_fields = Vec::new();
        for (name_index, (_, _, width, height)) in TEXTURE_EXPORTS.iter().enumerate() {
            push_i32(&mut decrypted, 0); // class
            push_i32(&mut decrypted, 0); // super
            push_i32(&mut decrypted, 0); // package
            push_i32(&mut decrypted, name_index as i32);
            push_i32(&mut decrypted, 0); // FName number
            push_i32(&mut decrypted, 0);
            push_u64(&mut decrypted, 0);
            push_i32(&mut decrypted, (32 + width * height + 60) as i32);
            serial_offset_fields.push(decrypted.len());
            push_i64(&mut decrypted, 0);
            push_i32(&mut decrypted, 0);
            push_i32(&mut decrypted, 0); // net objects
            decrypted.extend_from_slice(&[0u8; 16]);
            push_i32(&mut decrypted, 0);
        }

        let chunk_info_offset = decrypted.len();
        push_i32(&mut decrypted, 2);
        let first_chunk_entry = decrypted.len();
        decrypted.extend_from_slice(&[0u8; 36 * 2]);
        while decrypted.len() % 16 != 0 {
            decrypted.push(0);
        }

        let total_header_size = name_offset + decrypted.len();
        let mut first_payload = vec![0x5au8; 128];
        let mut serial_offsets = Vec::new();
        for (index, (_, _, width, height)) in TEXTURE_EXPORTS.iter().enumerate() {
            let serial_size = 32 + width * height + 60;
            let serial_start = first_payload.len();
            serial_offsets.push(total_header_size + serial_start);
            first_payload.extend(std::iter::repeat_n(0x80 + index as u8, 32));
            first_payload
                .extend((0..width * height).map(|byte| ((byte * 17 + index * 37) % 251) as u8));
            first_payload.extend(std::iter::repeat_n(0x40 + index as u8, 60));
            assert_eq!(first_payload.len() - serial_start, serial_size);
            first_payload.extend_from_slice(&[0xa5; 17]);
        }
        first_payload.extend_from_slice(&[0x6c; 256]);
        let second_payload: Vec<u8> = (0..32_777)
            .map(|index| ((index * 29 + 11) % 251) as u8)
            .collect();

        for (field, offset) in serial_offset_fields.into_iter().zip(serial_offsets) {
            set_i64(&mut decrypted, field, offset as i64);
        }

        let first_compressed =
            compress_into_chunk(&first_payload, 131_072).expect("compress first fixture chunk");
        let second_compressed =
            compress_into_chunk(&second_payload, 131_072).expect("compress second fixture chunk");
        let first_compressed_offset = total_header_size;
        let second_compressed_offset = first_compressed_offset + first_compressed.len();

        let second_chunk_entry = first_chunk_entry + 36;
        set_i64(&mut decrypted, first_chunk_entry, total_header_size as i64);
        set_i32(
            &mut decrypted,
            first_chunk_entry + 8,
            first_payload.len() as i32,
        );
        set_i64(
            &mut decrypted,
            first_chunk_entry + 12,
            first_compressed_offset as i64,
        );
        set_i32(
            &mut decrypted,
            first_chunk_entry + 20,
            first_compressed.len() as i32,
        );
        set_i64(
            &mut decrypted,
            second_chunk_entry,
            (total_header_size + first_payload.len()) as i64,
        );
        set_i32(
            &mut decrypted,
            second_chunk_entry + 8,
            second_payload.len() as i32,
        );
        set_i64(
            &mut decrypted,
            second_chunk_entry + 12,
            second_compressed_offset as i64,
        );
        set_i32(
            &mut decrypted,
            second_chunk_entry + 20,
            second_compressed.len() as i32,
        );

        set_i32(
            &mut summary,
            total_header_size_pos,
            total_header_size as i32,
        );
        set_i32(&mut summary, name_offset_pos, name_offset as i32);
        set_i32(&mut summary, export_offset_pos, export_offset as i32);
        set_i32(
            &mut summary,
            chunk_info_offset_pos,
            chunk_info_offset as i32,
        );

        let encrypted = aes_encrypt_ecb(&decrypted, &TEST_KEY).expect("encrypt fixture header");
        let mut raw = summary;
        raw.extend_from_slice(&encrypted);
        assert_eq!(raw.len(), total_header_size);
        raw.extend_from_slice(&first_compressed);
        raw.extend_from_slice(&second_compressed);
        Fixture { raw }
    }

    fn test_root(label: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "hebnix-boost-{label}-{}-{unique}",
            std::process::id()
        ))
    }

    fn write_test_keys(path: &Path) {
        fs::write(
            path,
            format!(
                "# test keys\n{}\n{}\n",
                BASE64_STANDARD.encode(WRONG_KEY),
                BASE64_STANDARD.encode(TEST_KEY)
            ),
        )
        .expect("write test key file");
    }

    fn png_bytes(color: [u8; 4]) -> Vec<u8> {
        let image = image::RgbaImage::from_pixel(8, 8, image::Rgba(color));
        let mut output = Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(image)
            .write_to(&mut output, image::ImageFormat::Png)
            .expect("encode test PNG");
        output.into_inner()
    }

    #[test]
    fn compressed_chunk_round_trips_multiple_blocks() {
        let block_size = 262_144;
        let payload: Vec<u8> = (0..block_size * 2 + 123)
            .map(|index| ((index * 31) % 251) as u8)
            .collect();
        let chunk = compress_into_chunk(&payload, block_size).expect("compress chunk");
        let (decompressed, block_size, end) =
            upk::decomp_chunk_at(&chunk, 0).expect("decompress generated chunk");

        assert_eq!(decompressed, payload);
        assert_eq!(block_size as usize, 262_144);
        assert_eq!(end, chunk.len());
    }

    #[test]
    fn encrypted_package_uses_external_key_and_padded_chunk_rows() {
        let root = test_root("key-layout");
        fs::create_dir_all(&root).expect("create test root");
        let key_file = root.join("test_keys.txt");
        write_test_keys(&key_file);
        let fixture = build_encrypted_fixture();

        let package = EncryptedPackage::load(fixture.raw, &key_file).expect("load fixture");
        assert_eq!(package.key, TEST_KEY);
        assert_eq!(package.key_line, 3);
        assert_eq!(package.chunks.len(), 2);
        assert_eq!(
            package.chunks[1].table_entry_offset - package.chunks[0].table_entry_offset,
            36
        );
        for (_, export_name, _, _) in TEXTURE_EXPORTS {
            assert!(package.names.iter().any(|name| name == export_name));
        }
        fs::remove_dir_all(root).expect("remove test root");
    }

    #[test]
    fn patch_boost_meter_reencrypts_and_replaces_selected_exports() {
        let root = test_root("patcher");
        let game_dir = root.join("game");
        let backup_dir = root.join("backup");
        let key_file = root.join("test_keys.txt");
        fs::create_dir_all(&game_dir).expect("create test game directory");
        write_test_keys(&key_file);

        let fixture = build_encrypted_fixture();
        fs::write(game_dir.join(GFX_UPK), &fixture.raw).expect("write test UPK");
        let original =
            EncryptedPackage::load(fixture.raw.clone(), &key_file).expect("load original fixture");

        let mut pack = HashMap::new();
        pack.insert("Background".to_string(), png_bytes([220, 20, 40, 255]));
        pack.insert("Fill".to_string(), png_bytes([10, 180, 240, 160]));
        patch_boost_meter(
            game_dir.to_str().expect("game path"),
            backup_dir.to_str().expect("backup path"),
            &key_file,
            &pack,
        )
        .expect("patch boost meter");

        let output = fs::read(game_dir.join(GFX_UPK)).expect("read patched UPK");
        let patched =
            EncryptedPackage::load(output.clone(), &key_file).expect("load repacked fixture");
        assert_eq!(patched.key, TEST_KEY);
        let original_header_end = original.header.name_offset + original.encrypted_header_size;
        assert_ne!(
            &output[original.header.name_offset..original_header_end],
            &fixture.raw[original.header.name_offset..original_header_end],
            "the changed chunk table must be re-encrypted with the matched key"
        );

        for (key, export_name, width, height) in TEXTURE_EXPORTS {
            let (start, end) = patched
                .texture_range(export_name, width, height)
                .expect("locate patched texture");
            if let Some(png) = pack.get(key) {
                let image = image::load_from_memory(png)
                    .expect("decode test PNG")
                    .to_rgba8();
                let resized = resize_rgba_isolated(&image, width as u32, height as u32);
                let expected = image_to_dxt5_with_alpha(&resized, width, height);
                assert_eq!(&patched.logical_data[start..end], expected.as_slice());
            } else {
                let (original_start, original_end) = original
                    .texture_range(export_name, width, height)
                    .expect("locate original texture");
                assert_eq!(
                    &patched.logical_data[start..end],
                    &original.logical_data[original_start..original_end]
                );
            }
        }

        // A replacement always starts from the pristine encrypted backup.
        pack.insert("Background".to_string(), png_bytes([30, 210, 70, 220]));
        pack.insert("Fill".to_string(), png_bytes([245, 170, 15, 255]));
        patch_boost_meter(
            game_dir.to_str().expect("game path"),
            backup_dir.to_str().expect("backup path"),
            &key_file,
            &pack,
        )
        .expect("replace active boost meter");
        let replaced_output = fs::read(game_dir.join(GFX_UPK)).expect("read replaced UPK");
        let replaced =
            EncryptedPackage::load(replaced_output, &key_file).expect("load replacement package");
        for (key, export_name, width, height) in TEXTURE_EXPORTS {
            let (start, end) = replaced
                .texture_range(export_name, width, height)
                .expect("locate replacement texture");
            if let Some(png) = pack.get(key) {
                let image = image::load_from_memory(png)
                    .expect("decode replacement PNG")
                    .to_rgba8();
                let resized = resize_rgba_isolated(&image, width as u32, height as u32);
                let expected = image_to_dxt5_with_alpha(&resized, width, height);
                assert_eq!(&replaced.logical_data[start..end], expected.as_slice());
            } else {
                let (original_start, original_end) = original
                    .texture_range(export_name, width, height)
                    .expect("locate original texture");
                assert_eq!(
                    &replaced.logical_data[start..end],
                    &original.logical_data[original_start..original_end]
                );
            }
        }

        let backup =
            fs::read(backup_dir.join(format!("{}.bak", GFX_UPK))).expect("read pristine backup");
        assert_eq!(backup, fixture.raw);
        fs::remove_dir_all(&root).expect("remove test directory");
    }

    #[test]
    fn patcher_refuses_a_key_file_without_the_package_key() {
        let root = test_root("wrong-key");
        let game_dir = root.join("game");
        let backup_dir = root.join("backup");
        let key_file = root.join("test_keys.txt");
        fs::create_dir_all(&game_dir).expect("create test game directory");
        fs::write(
            &key_file,
            format!("{}\n", BASE64_STANDARD.encode(WRONG_KEY)),
        )
        .expect("write wrong key file");
        fs::write(game_dir.join(GFX_UPK), build_encrypted_fixture().raw).expect("write test UPK");
        let mut pack = HashMap::new();
        pack.insert("Background".to_string(), png_bytes([1, 2, 3, 255]));

        let error = patch_boost_meter(
            game_dir.to_str().expect("game path"),
            backup_dir.to_str().expect("backup path"),
            &key_file,
            &pack,
        )
        .expect_err("wrong key must be rejected");
        assert!(error.contains("None of the 1 available keys"));
        fs::remove_dir_all(root).expect("remove test root");
    }
}
