//! RL .save file io: decrypt, parse, optionally re-encrypt.

use std::path::Path;

use serde_json::{Value, json};

use crate::save_file::binary_parser::{parse_property_stream, read_ue3};
use crate::save_file::binary_serializer::{serialize_property_stream, write_ue3};
use crate::save_file::crypto::{OBJHEADER, SaveError, aes_decrypt, aes_encrypt, crc32};

/// raw parsed save (dynamic tree)
#[derive(Debug, Clone)]
pub struct RawSave {
    pub source_file: String,
    pub encrypted_size: u32,
    pub crc_expected: u32,
    pub crc_calculated: u32,
    pub crc_match: bool,
    pub foosball: u32,
    pub magic: u32,
    pub engine_version: i32,
    pub licensee_version: i32,
    pub type_version: i32,
    pub object_types: Vec<ObjectType>,
    /// root property stream
    pub properties: Value,
    /// per-object property trees, each with a "__type" key
    pub objects: Vec<Value>,
}

#[derive(Debug, Clone)]
pub struct ObjectType {
    pub type_name: String,
    pub object_index: u32,
    pub file_position: u32,
}

fn read_i32(data: &[u8], offset: usize) -> Result<i32, SaveError> {
    data.get(offset..offset + 4)
        .map(|b| i32::from_le_bytes(b.try_into().unwrap()))
        .ok_or_else(|| SaveError::Parse(format!("i32 out of range at {offset}")))
}

fn read_u32(data: &[u8], offset: usize) -> Result<u32, SaveError> {
    data.get(offset..offset + 4)
        .map(|b| u32::from_le_bytes(b.try_into().unwrap()))
        .ok_or_else(|| SaveError::Parse(format!("u32 out of range at {offset}")))
}

/// decrypt + parse a RL .save
pub fn parse_savedata(filepath: &Path, check_crc: bool) -> Result<RawSave, SaveError> {
    let raw = std::fs::read(filepath)?;

    let part1_len = read_u32(&raw, 0)? as usize;
    let part1_crc = read_u32(&raw, 4)?;
    let encrypted = raw
        .get(8..8 + part1_len)
        .ok_or_else(|| SaveError::Parse("encrypted payload out of range".into()))?;
    let crc_actual = crc32(encrypted);
    let crc_ok = part1_crc == crc_actual;

    if check_crc && !crc_ok {
        tracing::warn!("CRC mismatch: expected 0x{part1_crc:08X}, got 0x{crc_actual:08X}");
    }

    let dec = aes_decrypt(encrypted);
    let mut off = 0usize;
    let foosball = read_u32(&dec, off)?;
    off += 4;
    let magic = read_u32(&dec, off)?;
    off += 4;
    let eng = read_i32(&dec, off)?;
    off += 4;
    let lic = read_i32(&dec, off)?;
    off += 4;
    let typv = read_i32(&dec, off)?;
    off += 4;
    let svlen = read_i32(&dec, off)? as usize;
    off += 4;
    let svdata = dec
        .get(off..off + svlen - 4)
        .ok_or_else(|| SaveError::Parse("savedata out of range".into()))?
        .to_vec();
    off += svlen - 4;

    let ntypes = read_i32(&dec, off)?;
    off += 4;
    let mut objtypes: Vec<ObjectType> = Vec::new();
    for _ in 0..ntypes {
        let (tn, new_off) = read_ue3(&dec, off).map_err(SaveError::Parse)?;
        off = new_off;
        let fp = read_u32(&dec, off)?;
        off += 4;
        let oi = read_u32(&dec, off)?;
        off += 4;
        objtypes.push(ObjectType {
            type_name: tn,
            object_index: oi,
            file_position: fp,
        });
    }

    // Root property stream (skip OBJHEADER at start of savedata)
    let (props, _) = parse_property_stream(&svdata, 4).map_err(SaveError::Parse)?;

    let mut objects: Vec<Value> = Vec::new();
    for i in 0..objtypes.len() {
        let ot = &objtypes[i];
        let obj_pos = ot.file_position as i64 - 4;
        if obj_pos < 0 || obj_pos as usize >= svdata.len() {
            objects.push(json!({
                "__type": ot.type_name,
                "__error": "out of range",
            }));
            continue;
        }
        let obj_pos = obj_pos as usize;
        let end = if i + 1 < objtypes.len() {
            (objtypes[i + 1].file_position as i64 - 4).max(0) as usize
        } else {
            svdata.len()
        };
        let end = end.min(svdata.len());
        let start = (obj_pos + 4).min(end);
        let obj_bytes = &svdata[start..end]; // skip per-object OBJHEADER

        match parse_property_stream(obj_bytes, 0) {
            Ok((mut oprop, _)) => {
                if let Some(obj) = oprop.as_object_mut() {
                    obj.insert("__type".to_string(), json!(ot.type_name));
                }
                objects.push(oprop);
            }
            Err(e) => {
                objects.push(json!({
                    "__type": ot.type_name,
                    "__parse_error": e,
                    "__raw_hex": hex_encode(obj_bytes),
                }));
            }
        }
    }

    Ok(RawSave {
        source_file: filepath
            .file_name()
            .map(|f| f.to_string_lossy().to_string())
            .unwrap_or_default(),
        encrypted_size: part1_len as u32,
        crc_expected: part1_crc,
        crc_calculated: crc_actual,
        crc_match: crc_ok,
        foosball,
        magic,
        engine_version: eng,
        licensee_version: lic,
        type_version: typv,
        object_types: objtypes,
        properties: props,
        objects,
    })
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// experimental: serialize a parsed save back into a .save. round-tripped
/// files can differ in size from the original, the game may not accept them.
pub fn assemble_savedata(data: &RawSave, output_path: &Path) -> Result<(), SaveError> {
    let prop_bytes = serialize_property_stream(&data.properties);
    let prop_len = prop_bytes.len() + 4; // + OBJHEADER

    let obj_blobs: Vec<Vec<u8>> = data
        .objects
        .iter()
        .map(|obj| {
            let props: serde_json::Map<String, Value> = obj
                .as_object()
                .map(|o| {
                    o.iter()
                        .filter(|(k, _)| *k != "__type")
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect()
                })
                .unwrap_or_default();
            serialize_property_stream(&Value::Object(props))
        })
        .collect();

    let mut sd: Vec<u8> = Vec::new();
    sd.extend_from_slice(&OBJHEADER.to_le_bytes());
    sd.extend_from_slice(&prop_bytes);

    let mut new_ot: Vec<ObjectType> = Vec::new();
    let mut pos = prop_len;
    for (i, blob) in obj_blobs.iter().enumerate() {
        let fp = pos;
        pos += 4 + blob.len();
        new_ot.push(ObjectType {
            type_name: data
                .object_types
                .get(i)
                .map(|o| o.type_name.clone())
                .unwrap_or_else(|| "Unknown".to_string()),
            object_index: data
                .object_types
                .get(i)
                .map(|o| o.object_index)
                .unwrap_or(i as u32),
            file_position: fp as u32 + 4,
        });
        sd.extend_from_slice(&OBJHEADER.to_le_bytes());
        sd.extend_from_slice(blob);
    }
    let ot = if obj_blobs.is_empty() {
        data.object_types.clone()
    } else {
        new_ot
    };

    let savedata_len = sd.len() as i32 + 4;

    let mut buf: Vec<u8> = Vec::new();
    buf.extend_from_slice(&0xF005_BA11u32.to_le_bytes());
    buf.extend_from_slice(&0x7FFF_FFFFu32.to_le_bytes());
    buf.extend_from_slice(&data.engine_version.to_le_bytes());
    buf.extend_from_slice(&data.licensee_version.to_le_bytes());
    buf.extend_from_slice(&data.type_version.to_le_bytes());
    buf.extend_from_slice(&savedata_len.to_le_bytes());
    buf.extend_from_slice(&sd);
    buf.extend_from_slice(&(ot.len() as i32).to_le_bytes());
    for o in &ot {
        buf.extend_from_slice(&write_ue3(&o.type_name));
        buf.extend_from_slice(&o.file_position.to_le_bytes());
        buf.extend_from_slice(&o.object_index.to_le_bytes());
    }

    let encrypted = aes_encrypt(&buf);
    let crc = crc32(&encrypted);
    let mut out: Vec<u8> = Vec::with_capacity(encrypted.len() + 8);
    out.extend_from_slice(&(encrypted.len() as u32).to_le_bytes());
    out.extend_from_slice(&crc.to_le_bytes());
    out.extend_from_slice(&encrypted);

    std::fs::write(output_path, out)?;
    Ok(())
}
