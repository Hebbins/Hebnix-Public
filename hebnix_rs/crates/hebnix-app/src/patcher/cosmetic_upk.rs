use aes::Aes256;
use aes::cipher::{BlockDecrypt, BlockEncrypt, KeyInit, generic_array::GenericArray};
use std::fs;
use std::path::Path;

const UPK_MAGIC: u32 = 2_653_586_369;
const DEFAULT_KEY: [u8; 32] = [
    199, 223, 107, 19, 37, 42, 204, 113, 71, 187, 81, 201, 138, 215, 227, 75, 127, 229, 0, 183,
    127, 165, 250, 178, 147, 226, 242, 78, 107, 23, 231, 121,
];

#[derive(Clone, Copy)]
struct UpkInfo {
    licensee_version: u16,
    total_header_size: usize,
    name_count: usize,
    name_offset: usize,
    export_count: usize,
    export_offset: usize,
    package_guid_offset: usize,
    generation_size: i32,
    generation_size_offset: usize,
    compressed_chunk_info_offset: usize,
    compressed_chunk_info_offset_field: usize,
}

#[derive(Clone)]
struct NameEntry {
    offset: usize,
    len: usize,
    name: String,
}

struct SummaryCursor<'a> {
    data: &'a [u8],
    position: usize,
}

impl<'a> SummaryCursor<'a> {
    fn new(data: &'a [u8], position: usize) -> Self {
        Self { data, position }
    }

    fn i32(&mut self) -> Result<i32, String> {
        let value = read_i32(self.data, self.position)?;
        self.position += 4;
        Ok(value)
    }

    fn skip(&mut self, bytes: usize) -> Result<(), String> {
        self.position = self
            .position
            .checked_add(bytes)
            .ok_or("UPK summary offset overflow")?;
        if self.position > self.data.len() {
            return Err("UPK summary is truncated".into());
        }
        Ok(())
    }

    fn fstring(&mut self) -> Result<(), String> {
        let length = self.i32()?;
        let bytes = if length < 0 {
            usize::try_from(-i64::from(length))
                .map_err(|_| "Invalid wide UPK string length")?
                .checked_mul(2)
                .ok_or("UPK string length overflow")?
        } else {
            usize::try_from(length).map_err(|_| "Invalid UPK string length")?
        };
        self.skip(bytes)
    }

    fn array(&mut self, element_size: usize) -> Result<(), String> {
        let count = usize::try_from(self.i32()?).map_err(|_| "Negative UPK array count")?;
        self.skip(
            count
                .checked_mul(element_size)
                .ok_or("UPK array length overflow")?,
        )
    }
}

fn read_i32(data: &[u8], offset: usize) -> Result<i32, String> {
    let bytes = data
        .get(offset..offset + 4)
        .ok_or("UPK header is truncated")?;
    Ok(i32::from_le_bytes(bytes.try_into().unwrap()))
}

fn write_i32(data: &mut [u8], offset: usize, value: i32) -> Result<(), String> {
    let output = data
        .get_mut(offset..offset + 4)
        .ok_or("UPK write is outside the file")?;
    output.copy_from_slice(&value.to_le_bytes());
    Ok(())
}

fn write_i64(data: &mut [u8], offset: usize, value: i64) -> Result<(), String> {
    let output = data
        .get_mut(offset..offset + 8)
        .ok_or("UPK write is outside the decrypted header")?;
    output.copy_from_slice(&value.to_le_bytes());
    Ok(())
}

fn read_i64(data: &[u8], offset: usize) -> Result<i64, String> {
    let bytes = data
        .get(offset..offset + 8)
        .ok_or("UPK read is outside the decrypted header")?;
    Ok(i64::from_le_bytes(bytes.try_into().unwrap()))
}

fn chunk_table_stride(data: &[u8], table: usize, count: usize) -> Result<usize, String> {
    let normal = 24usize;
    let padded = 36usize;
    if count >= 2 {
        let first = read_i64(data, table + 4)?;
        let first_size = i64::from(read_i32(data, table + 12)?);
        let expected_second = first
            .checked_add(first_size)
            .ok_or("Compressed chunk offset overflow")?;
        if read_i64(data, table + 4 + padded).ok() == Some(expected_second) {
            return Ok(padded);
        }
    }
    Ok(normal)
}

fn read_info(data: &[u8]) -> Result<UpkInfo, String> {
    if data.len() < 57 || u32::from_le_bytes(data[0..4].try_into().unwrap()) != UPK_MAGIC {
        return Err("Invalid UPK magic number".into());
    }
    let total_header_size =
        usize::try_from(read_i32(data, 8)?).map_err(|_| "Negative total header size")?;
    let licensee_version = u16::from_le_bytes(data[6..8].try_into().unwrap());
    let name_count = usize::try_from(read_i32(data, 25)?).map_err(|_| "Negative name count")?;
    let name_offset = usize::try_from(read_i32(data, 29)?).map_err(|_| "Negative name offset")?;
    let export_count = usize::try_from(read_i32(data, 33)?).map_err(|_| "Negative export count")?;
    let export_offset =
        usize::try_from(read_i32(data, 37)?).map_err(|_| "Negative export offset")?;
    if name_offset < 12 || name_offset > data.len() {
        return Err(format!("UPK name offset {name_offset} is outside the file"));
    }
    // Parse the variable-length package summary. Reading these values at
    // name_offset-12 only works for one summary shape; on other valid packages
    // it reads padding zeros and later rewrites name-table bytes as chunk rows.
    let mut summary = SummaryCursor::new(data, 12);
    summary.fstring()?;
    summary.i32()?; // package flags
    for _ in 0..7 {
        summary.i32()?;
    }
    for _ in 0..4 {
        summary.i32()?;
    }
    let package_guid_offset = summary.position;
    summary.skip(16)?;
    summary.array(12)?;
    for _ in 0..3 {
        summary.i32()?;
    }
    summary.array(16)?;
    summary.i32()?;
    let string_count =
        usize::try_from(summary.i32()?).map_err(|_| "Negative summary string count")?;
    if string_count > 100_000 {
        return Err("UPK summary has too many strings".into());
    }
    for _ in 0..string_count {
        summary.fstring()?;
    }
    let entry_count =
        usize::try_from(summary.i32()?).map_err(|_| "Negative summary entry count")?;
    if entry_count > 100_000 {
        return Err("UPK summary has too many entries".into());
    }
    for _ in 0..entry_count {
        summary.skip(20)?;
        summary.array(4)?;
    }
    let generation_size_offset = summary.position;
    let generation_size = summary.i32()?;
    let compressed_chunk_info_offset_field = summary.position;
    let compressed_chunk_info_offset =
        usize::try_from(summary.i32()?).map_err(|_| "Negative compressed chunk table offset")?;
    summary.i32()?;
    if summary.position != name_offset && summary.position.checked_add(12) != Some(name_offset) {
        return Err(format!(
            "UPK summary fields end at {}, but name table starts at {name_offset}",
            summary.position
        ));
    }
    Ok(UpkInfo {
        licensee_version,
        total_header_size,
        name_count,
        name_offset,
        export_count,
        export_offset,
        package_guid_offset,
        generation_size,
        generation_size_offset,
        compressed_chunk_info_offset,
        compressed_chunk_info_offset_field,
    })
}

fn exported_name_indexes(data: &[u8], info: UpkInfo) -> Result<Vec<usize>, String> {
    let mut position = info
        .export_offset
        .checked_sub(info.name_offset)
        .ok_or("Export table precedes encrypted header")?;
    let mut names = Vec::with_capacity(info.export_count);
    for index in 0..info.export_count {
        let object_name = usize::try_from(read_i32(data, position + 12)?)
            .map_err(|_| format!("Export {index} has a negative object-name index"))?;
        if object_name >= info.name_count {
            return Err(format!("Export {index} has an invalid object-name index"));
        }
        names.push(object_name);

        // FObjectExport is variable length because it ends with an array of
        // net-object indices. This mirrors the UE3/C# package reader.
        position = position
            .checked_add(if info.licensee_version >= 22 { 48 } else { 44 })
            .ok_or("Export table offset overflow")?;
        let net_object_count = usize::try_from(read_i32(data, position)?)
            .map_err(|_| format!("Export {index} has a negative net-object count"))?;
        position = position
            .checked_add(4 + net_object_count.saturating_mul(4) + 16 + 4)
            .ok_or("Export table offset overflow")?;
        if position > data.len() {
            return Err("Export table is truncated".into());
        }
    }
    Ok(names)
}

fn encrypted_size(info: UpkInfo) -> Result<(usize, usize), String> {
    let plain = usize::try_from(
        info.total_header_size as i64 - info.generation_size as i64 - info.name_offset as i64,
    )
    .map_err(|_| "Invalid encrypted UPK header size")?;
    Ok((plain, plain.div_ceil(16) * 16))
}

fn crypt(data: &[u8], key: &[u8; 32], encrypt: bool) -> Result<Vec<u8>, String> {
    if !data.len().is_multiple_of(16) {
        return Err("AES region is not block aligned".into());
    }
    let cipher = Aes256::new(GenericArray::from_slice(key));
    let mut output = data.to_vec();
    for chunk in output.chunks_exact_mut(16) {
        if encrypt {
            cipher.encrypt_block(GenericArray::from_mut_slice(chunk));
        } else {
            cipher.decrypt_block(GenericArray::from_mut_slice(chunk));
        }
    }
    Ok(output)
}

fn keys(base_dir: &Path) -> Vec<[u8; 32]> {
    let mut keys = vec![DEFAULT_KEY];
    let _ = base_dir;
    if let Ok(embedded) = crate::upk_keys::embedded() {
        for (_, key) in embedded {
            if !keys.contains(&key) {
                keys.push(key);
            }
        }
    }
    keys
}

fn scan_names(data: &[u8], count: usize) -> Result<Vec<NameEntry>, String> {
    let mut entries = Vec::with_capacity(count);
    let mut offset = 0usize;
    for index in 0..count {
        let len = usize::try_from(read_i32(data, offset)?)
            .map_err(|_| format!("Name table entry {index} has a negative length"))?;
        if len == 0 || len > 512 || offset + 4 + len + 8 > data.len() {
            return Err(format!(
                "Name table entry {index} has invalid length {len} at {offset}"
            ));
        }
        let raw_name = &data[offset + 4..offset + 4 + len.saturating_sub(1)];
        let name: String = raw_name.iter().map(|byte| char::from(*byte)).collect();
        entries.push(NameEntry { offset, len, name });
        offset += 4 + len + 8;
    }
    Ok(entries)
}

fn find_key_and_decrypt(
    raw: &[u8],
    info: UpkInfo,
    candidates: &[[u8; 32]],
) -> Result<(Vec<u8>, [u8; 32], usize), String> {
    let (_, aligned) = encrypted_size(info)?;
    let encrypted = raw
        .get(info.name_offset..info.name_offset + aligned)
        .ok_or("UPK is too small for its encrypted header")?;
    for key in candidates {
        let decrypted = crypt(encrypted, key, false)?;
        if scan_names(&decrypted, info.name_count).is_ok() {
            return Ok((decrypted, *key, aligned));
        }
    }
    Err("No embedded key could decrypt this cosmetic UPK".into())
}

fn replace_name(
    data: Vec<u8>,
    old: &str,
    new: &str,
    count: usize,
) -> Result<(Vec<u8>, i32), String> {
    let Some(entry) = scan_names(&data, count)?
        .into_iter()
        .find(|entry| entry.name.trim_end_matches('\0') == old)
    else {
        return Ok((data, 0));
    };
    let mut replacement = new.as_bytes().to_vec();
    replacement.push(0);
    if replacement.len() <= entry.len {
        let mut output = data;
        output[entry.offset + 4..entry.offset + 4 + entry.len].fill(0);
        output[entry.offset + 4..entry.offset + 4 + replacement.len()]
            .copy_from_slice(&replacement);
        return Ok((output, 0));
    }
    let delta = replacement.len() - entry.len;
    let mut output = Vec::with_capacity(data.len() + delta);
    output.extend_from_slice(&data[..entry.offset]);
    output.extend_from_slice(&(replacement.len() as i32).to_le_bytes());
    output.extend_from_slice(&replacement);
    output.extend_from_slice(&data[entry.offset + 4 + entry.len..]);
    Ok((output, delta as i32))
}

fn apply_deltas(
    raw: &mut [u8],
    decrypted: &mut [u8],
    info: UpkInfo,
    delta: i32,
) -> Result<(), String> {
    if delta == 0 {
        return Ok(());
    }
    let (old_plain, old_aligned) = encrypted_size(info)?;
    let new_aligned = usize::try_from(old_plain as i64 + delta as i64)
        .map_err(|_| "Renamed header became negative")?
        .div_ceil(16)
        * 16;
    let alignment_delta = new_aligned as i64 - old_aligned as i64;
    write_i32(
        raw,
        info.compressed_chunk_info_offset_field,
        i32::try_from(info.compressed_chunk_info_offset as i64 + delta as i64)
            .map_err(|_| "Chunk table offset overflow")?,
    )?;
    for offset in [37usize, 45, 49, 53] {
        let value = read_i32(raw, offset)?;
        if value > info.name_offset as i32 {
            write_i32(raw, offset, value + delta)?;
        }
    }
    write_i32(
        raw,
        info.generation_size_offset,
        i32::try_from(info.generation_size as i64 + alignment_delta - delta as i64)
            .map_err(|_| "Generation size overflow")?,
    )?;
    if alignment_delta != 0 {
        let table = usize::try_from(info.compressed_chunk_info_offset as i64 + delta as i64)
            .map_err(|_| "Chunk table offset became negative")?;
        let count = usize::try_from(read_i32(decrypted, table)?)
            .map_err(|_| "Negative compressed chunk count")?;
        let stride = chunk_table_stride(decrypted, table, count)?;
        let mut row = table + 4;
        for _ in 0..count {
            let old = i64::from_le_bytes(
                decrypted
                    .get(row + 12..row + 20)
                    .ok_or("Compressed chunk table is truncated")?
                    .try_into()
                    .unwrap(),
            );
            write_i64(decrypted, row + 12, old + alignment_delta)?;
            row += stride;
        }
        write_i32(
            raw,
            8,
            i32::try_from(info.total_header_size as i64 + alignment_delta)
                .map_err(|_| "Total header size overflow")?,
        )?;
    }
    Ok(())
}

fn infer_explosion_pairs(
    donor_upk: &str,
    donor_asset: &str,
    target_upk: &str,
    target_asset: &str,
) -> Vec<(String, String)> {
    let donor: Vec<&str> = donor_asset.split('.').collect();
    let target: Vec<&str> = target_asset.split('.').collect();
    let target_lower: Vec<String> = target
        .iter()
        .map(|part| part.to_ascii_lowercase())
        .collect();
    let mut pairs = Vec::new();
    let mut seen = Vec::<String>::new();
    for index in 0..donor.len().min(target.len()) {
        let old = donor[index];
        let new = target[index];
        let important = index == 0
            || index + 1 == donor.len()
            || target_lower.contains(&old.to_ascii_lowercase());
        if important
            && !old.is_empty()
            && !new.is_empty()
            && old != new
            && !seen.iter().any(|x| x == old)
        {
            seen.push(old.to_string());
            pairs.push((old.to_string(), new.to_string()));
        }
    }
    let donor_stem = Path::new(donor_upk)
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or(donor_upk);
    let target_stem = Path::new(target_upk)
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or(target_upk);
    if donor_stem != target_stem && !seen.iter().any(|value| value == donor_stem) {
        pairs.push((donor_stem.to_string(), target_stem.to_string()));
    }
    pairs
}

fn logical_upk_stem(path: &Path) -> Option<&str> {
    let name = path.file_name()?.to_str()?;
    let without_backup = name.strip_suffix(".bak").unwrap_or(name);
    without_backup.strip_suffix(".upk").or_else(|| {
        Path::new(without_backup)
            .file_stem()
            .and_then(|value| value.to_str())
    })
}

fn validate_output(
    output: &[u8],
    target_key: &[u8; 32],
    target_name: &str,
    target_short: &str,
    target_export_name: &str,
    target_guid: &[u8; 16],
    alternative_targets: &[String],
) -> Result<(), String> {
    let info = read_info(output)?;
    if output.get(info.package_guid_offset..info.package_guid_offset + 16)
        != Some(target_guid.as_slice())
    {
        return Err("Generated UPK does not preserve the target package GUID".into());
    }
    let (_, aligned) = encrypted_size(info)?;
    let encrypted = output
        .get(info.name_offset..info.name_offset + aligned)
        .ok_or("Generated UPK encrypted header is truncated")?;
    let decrypted = crypt(encrypted, target_key, false)?;
    let names = scan_names(&decrypted, info.name_count)?;
    let export_names = exported_name_indexes(&decrypted, info)?;
    let has_target_name = names
        .iter()
        .any(|entry| entry.name.trim_end_matches('\0') == target_name);
    let has_target_short = target_short == target_name
        || names
            .iter()
            .any(|entry| entry.name.trim_end_matches('\0') == target_short);
    let has_alternative = alternative_targets.iter().any(|target| {
        names
            .iter()
            .any(|entry| entry.name.trim_end_matches('\0') == target)
    });
    if !has_target_name && !has_alternative {
        return Err(format!(
            "Generated UPK does not contain target package identity '{}'{}",
            target_name,
            if alternative_targets.is_empty() {
                String::new()
            } else {
                format!(" or one of: {}", alternative_targets.join(", "))
            }
        ));
    }
    if has_target_name && !has_target_short && !has_alternative {
        return Err(format!(
            "Generated UPK does not contain target package name '{target_short}'"
        ));
    }
    if !export_names.iter().any(|index| {
        names
            .get(*index)
            .is_some_and(|entry| entry.name.trim_end_matches('\0') == target_export_name)
    }) {
        return Err(format!(
            "Generated UPK does not export target object '{target_export_name}'"
        ));
    }

    let chunk_count = usize::try_from(read_i32(&decrypted, info.compressed_chunk_info_offset)?)
        .map_err(|_| "Generated UPK has a negative compressed-chunk count")?;
    if chunk_count == 0 || chunk_count >= 1_000 {
        return Err("Generated UPK has an invalid compressed-chunk count".into());
    }
    let mut row = info.compressed_chunk_info_offset + 4;
    let stride = chunk_table_stride(&decrypted, info.compressed_chunk_info_offset, chunk_count)?;
    let mut valid_chunks = 0usize;
    for index in 0..chunk_count {
        let compressed_offset = usize::try_from(i64::from_le_bytes(
            decrypted
                .get(row + 12..row + 20)
                .ok_or("Generated UPK compressed-chunk table is truncated")?
                .try_into()
                .unwrap(),
        ))
        .map_err(|_| format!("Generated UPK chunk {index} has a negative offset"))?;
        let (_, _, end) = crate::patch_core::upk::decomp_chunk_at(output, compressed_offset)
            .map_err(|error| {
                format!("Generated UPK chunk {index} at {compressed_offset} is invalid: {error:?}")
            })?;
        if end > output.len() {
            return Err(format!("Generated UPK chunk {index} extends past the file"));
        }
        valid_chunks += 1;
        row += stride;
    }

    if valid_chunks == 0 {
        return Err("Generated UPK contains no readable compressed chunks".into());
    }

    Ok(())
}

pub fn patch_for_target(
    source: &Path,
    target_backup: &Path,
    destination: &Path,
    base_dir: &Path,
    donor_asset_path: Option<&str>,
    target_asset_path: Option<&str>,
) -> Result<(), String> {
    let mut raw = fs::read(source).map_err(|error| format!("{}: {error}", source.display()))?;
    let info = read_info(&raw)?;
    let candidates = keys(base_dir);
    let (mut decrypted, source_key, old_encrypted_size) =
        find_key_and_decrypt(&raw, info, &candidates)?;
    let target_raw =
        fs::read(target_backup).map_err(|error| format!("{}: {error}", target_backup.display()))?;
    let target_info = read_info(&target_raw)?;
    let target_key = find_key_and_decrypt(&target_raw, target_info, &candidates)
        .ok()
        .map(|(_, key, _)| key)
        .unwrap_or(source_key);
    let target_guid: [u8; 16] = target_raw
        .get(target_info.package_guid_offset..target_info.package_guid_offset + 16)
        .ok_or("Target UPK package GUID is truncated")?
        .try_into()
        .unwrap();
    raw.get_mut(info.package_guid_offset..info.package_guid_offset + 16)
        .ok_or("Donor UPK package GUID is truncated")?
        .copy_from_slice(&target_guid);

    let source_name = logical_upk_stem(source).ok_or("Source UPK has no valid filename")?;
    let target_name = logical_upk_stem(destination).ok_or("Target UPK has no valid filename")?;
    let pairs =
        if let (Some(donor_asset), Some(target_asset)) = (donor_asset_path, target_asset_path) {
            infer_explosion_pairs(source_name, donor_asset, target_name, target_asset)
        } else {
            let source_short = source_name.strip_suffix("_SF").unwrap_or(source_name);
            let target_short = target_name.strip_suffix("_SF").unwrap_or(target_name);
            vec![
                (source_name.to_string(), target_name.to_string()),
                (source_short.to_string(), target_short.to_string()),
            ]
        };
    if !scan_names(&decrypted, info.name_count)?
        .iter()
        .any(|entry| {
            pairs
                .iter()
                .any(|(old, _)| entry.name.trim_end_matches('\0') == old)
        })
    {
        return Err(format!(
            "The donor package identity '{source_name}' was not found in its UPK name table"
        ));
    }
    let mut total_delta = 0i32;
    let alternative_targets: Vec<String> =
        if donor_asset_path.is_some() && target_asset_path.is_some() {
            pairs.iter().map(|(_, new)| new.clone()).collect()
        } else {
            Vec::new()
        };
    let target_short = target_name.strip_suffix("_SF").unwrap_or(target_name);
    let target_export_name = target_asset_path
        .and_then(|asset| asset.split('.').next_back())
        .filter(|name| !name.is_empty())
        .unwrap_or(target_short);
    for (old, new) in pairs {
        let (new_data, delta) = replace_name(decrypted, &old, &new, info.name_count)?;
        decrypted = new_data;
        total_delta = total_delta
            .checked_add(delta)
            .ok_or("UPK name table growth overflow")?;
    }
    apply_deltas(&mut raw, &mut decrypted, info, total_delta)?;
    let (old_plain, _) = encrypted_size(info)?;
    let new_plain = usize::try_from(old_plain as i64 + total_delta as i64)
        .map_err(|_| "Renamed UPK header became negative")?;
    let new_encrypted_size = new_plain.div_ceil(16) * 16;
    decrypted.resize(new_encrypted_size, 0);
    let encrypted = crypt(&decrypted[..new_encrypted_size], &target_key, true)?;
    let mut output = Vec::with_capacity(raw.len() - old_encrypted_size + new_encrypted_size);
    output.extend_from_slice(&raw[..info.name_offset]);
    output.extend_from_slice(&encrypted);
    output.extend_from_slice(&raw[info.name_offset + old_encrypted_size..]);
    validate_output(
        &output,
        &target_key,
        target_name,
        target_short,
        target_export_name,
        &target_guid,
        &alternative_targets,
    )?;

    let temp_name = format!(
        "{}.swapping.tmp",
        destination
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("cosmetic.upk")
    );
    let temp_path = destination.with_file_name(temp_name);
    fs::write(&temp_path, &output)
        .map_err(|error| format!("Failed to write {}: {error}", temp_path.display()))?;
    let written = fs::read(&temp_path)
        .map_err(|error| format!("Failed to verify {}: {error}", temp_path.display()))?;
    if let Err(error) = validate_output(
        &written,
        &target_key,
        target_name,
        target_short,
        target_export_name,
        &target_guid,
        &alternative_targets,
    ) {
        let _ = fs::remove_file(&temp_path);
        return Err(error);
    }
    let result = fs::copy(&temp_path, destination)
        .map_err(|error| format!("Failed to install {}: {error}", destination.display()));
    let _ = fs::remove_file(&temp_path);
    result.map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires a local Rocket League installation"]
    fn patches_air_strike_over_big_splash() {
        let cooked = Path::new(r"C:\Program Files\Epic Games\rocketleague\TAGame\CookedPCConsole");
        let source = cooked.join("explosion_missiles_SF.upk");
        let target = cooked.join("explosion_splash_SF.upk");
        if !source.is_file() || !target.is_file() {
            return;
        }
        let temp_dir = std::env::temp_dir().join("hebnix_explosion_splash_test");
        fs::create_dir_all(&temp_dir).unwrap();
        let temp = temp_dir.join("explosion_splash_SF.upk");
        let key_dir = Path::new(r"C:\Users\Harry\Downloads\src");
        patch_for_target(
            &source,
            &target,
            &temp,
            key_dir,
            Some("explosion_missiles.explosion_missiles"),
            Some("explosion_splash.explosion_splash"),
        )
        .unwrap();
        fs::remove_file(temp).unwrap();
        fs::remove_dir(temp_dir).unwrap();
    }

    #[test]
    #[ignore = "requires a local Rocket League installation"]
    fn patches_current_air_strike_over_poly_pop() {
        let cooked = Path::new(r"C:\Program Files\Epic Games\rocketleague\TAGame\CookedPCConsole");
        let source = cooked.join("explosion_missiles_SF.upk");
        let backup = cooked.join("Backups/explosion_polygon_SF.upk.bak");
        let target = if backup.is_file() {
            backup
        } else {
            cooked.join("explosion_polygon_SF.upk")
        };
        if !source.is_file() || !target.is_file() {
            return;
        }
        let temp_dir = std::env::temp_dir().join("hebnix_explosion_polygon_test");
        fs::create_dir_all(&temp_dir).unwrap();
        let temp = temp_dir.join("explosion_polygon_SF.upk");
        patch_for_target(
            &source,
            &target,
            &temp,
            Path::new(r"C:\Users\Harry\Downloads\src"),
            Some("explosion_missiles.explosion_missiles"),
            Some("explosion_polygon.explosion_polygon"),
        )
        .unwrap();
        fs::remove_file(temp).unwrap();
        fs::remove_dir(temp_dir).unwrap();
    }

    #[test]
    #[ignore = "requires a local Rocket League installation"]
    fn patches_body_to_octane_export_identity() {
        let cooked = Path::new(r"C:\Program Files\Epic Games\rocketleague\TAGame\CookedPCConsole");
        let source = cooked.join("Body_Aftershock_SF.upk");
        let target = cooked.join("Body_Octane_SF.upk");
        if !source.is_file() || !target.is_file() {
            return;
        }
        let temp_dir = std::env::temp_dir().join("hebnix_body_octane_test");
        fs::create_dir_all(&temp_dir).unwrap();
        let temp = temp_dir.join("Body_Octane_SF.upk");
        patch_for_target(
            &source,
            &target,
            &temp,
            Path::new(r"C:\Users\Harry\Downloads\src"),
            Some("Body_Aftershock.Body_Aftershock"),
            Some("Body_Octane.Body_Octane"),
        )
        .unwrap();
        fs::remove_file(temp).unwrap();
        fs::remove_dir(temp_dir).unwrap();
    }

    #[test]
    #[ignore = "requires a local Rocket League installation"]
    fn patches_wheel_and_thumbnail_to_target_identity() {
        let cooked = Path::new(r"C:\Program Files\Epic Games\rocketleague\TAGame\CookedPCConsole");
        let temp_dir = std::env::temp_dir().join("hebnix_wheel_swap_test");
        fs::create_dir_all(&temp_dir).unwrap();
        let key_dir = Path::new(r"C:\Users\Harry\Downloads\src");
        for (source_name, target_name) in [
            ("WHEEL_AlphaRim_SF.upk", "WHEEL_Vortex_SF.upk"),
            ("WHEEL_AlphaRim_T_SF.upk", "WHEEL_Vortex_T_SF.upk"),
        ] {
            let source = cooked.join(source_name);
            let target = cooked.join(target_name);
            if !source.is_file() || !target.is_file() {
                continue;
            }
            let output = temp_dir.join(target_name);
            patch_for_target(&source, &target, &output, key_dir, None, None).unwrap();
            fs::remove_file(output).unwrap();
        }
        fs::remove_dir(temp_dir).unwrap();
    }

    #[test]
    #[ignore = "requires a local Rocket League installation"]
    fn patches_explosion_thumbnail_internal_identity() {
        let cooked = Path::new(r"C:\Program Files\Epic Games\rocketleague\TAGame\CookedPCConsole");
        let source = cooked.join("Explosion_Missiles_T_SF.upk");
        let target = cooked.join("Explosion_Polygon_T_SF.upk");
        if !source.is_file() || !target.is_file() {
            return;
        }
        let temp_dir = std::env::temp_dir().join("hebnix_explosion_thumb_test");
        fs::create_dir_all(&temp_dir).unwrap();
        let output = temp_dir.join("Explosion_Polygon_T_SF.upk");
        patch_for_target(
            &source,
            &target,
            &output,
            Path::new(r"C:\Users\Harry\Downloads\src"),
            Some("explosion_missiles_TThumbnail.explosion_missiles_TThumbnail"),
            Some("explosion_polygon_TThumbnail.explosion_polygon_TThumbnail"),
        )
        .unwrap();
        fs::remove_file(output).unwrap();
        fs::remove_dir(temp_dir).unwrap();
    }
}
