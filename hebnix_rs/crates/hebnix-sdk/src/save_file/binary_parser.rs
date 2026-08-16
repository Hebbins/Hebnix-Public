//! ue3 binary property-stream parser. reads RL's tagged-property format into
//! serde_json::Value trees.

use std::collections::BTreeMap;

use serde_json::{Map, Value, json};

use crate::save_file::crypto::{OBJHEADER, is_type_tag};

type PResult<T> = Result<T, String>;

fn read_i32(data: &[u8], offset: usize) -> PResult<i32> {
    let bytes = data
        .get(offset..offset + 4)
        .ok_or_else(|| format!("i32 out of range at {offset}"))?;
    Ok(i32::from_le_bytes(bytes.try_into().unwrap()))
}

fn read_u32(data: &[u8], offset: usize) -> PResult<u32> {
    let bytes = data
        .get(offset..offset + 4)
        .ok_or_else(|| format!("u32 out of range at {offset}"))?;
    Ok(u32::from_le_bytes(bytes.try_into().unwrap()))
}

fn read_u64(data: &[u8], offset: usize) -> PResult<u64> {
    let bytes = data
        .get(offset..offset + 8)
        .ok_or_else(|| format!("u64 out of range at {offset}"))?;
    Ok(u64::from_le_bytes(bytes.try_into().unwrap()))
}

fn read_f32(data: &[u8], offset: usize) -> PResult<f32> {
    let bytes = data
        .get(offset..offset + 4)
        .ok_or_else(|| format!("f32 out of range at {offset}"))?;
    Ok(f32::from_le_bytes(bytes.try_into().unwrap()))
}

fn round6(x: f64) -> f64 {
    (x * 1e6).round() / 1e6
}

/// read a ue3 length-prefixed string. positive len = utf-8 (len includes the
/// null terminator), negative len = utf-16-le (2 bytes per char).
pub fn read_ue3(data: &[u8], offset: usize) -> PResult<(String, usize)> {
    let length = read_i32(data, offset)?;
    let offset = offset + 4;
    if length == 0 {
        return Ok((String::new(), offset));
    }
    if length < 0 {
        let byte_count = (-(length as i64)) as usize * 2;
        let mut raw = data
            .get(offset..offset + byte_count)
            .ok_or_else(|| format!("utf16 string out of range at {offset}"))?;
        if raw.len() >= 2 && raw[raw.len() - 2..] == [0, 0] {
            raw = &raw[..raw.len() - 2];
        }
        let units: Vec<u16> = raw
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        let s = String::from_utf16(&units).map_err(|e| format!("utf16 decode: {e}"))?;
        return Ok((s, offset + byte_count));
    }
    let len = length as usize;
    let raw = data
        .get(offset..offset + len - 1)
        .ok_or_else(|| format!("utf8 string out of range at {offset}"))?;
    let s = std::str::from_utf8(raw)
        .map_err(|e| format!("utf8 decode: {e}"))?
        .to_string();
    Ok((s, offset + len))
}

/// read tagged props until the "None" sentinel. fixed-size arrays use a vidx
/// (value index): repeated prop names get collected into a list keyed by vidx.
pub fn parse_property_stream(data: &[u8], mut offset: usize) -> PResult<(Value, usize)> {
    let mut props: Map<String, Value> = Map::new();
    let mut fixed: Vec<(String, BTreeMap<i64, Value>)> = Vec::new();

    fn fixed_entry<'a>(
        fixed: &'a mut Vec<(String, BTreeMap<i64, Value>)>,
        name: &str,
    ) -> &'a mut BTreeMap<i64, Value> {
        if let Some(pos) = fixed.iter().position(|(n, _)| n == name) {
            &mut fixed[pos].1
        } else {
            fixed.push((name.to_string(), BTreeMap::new()));
            &mut fixed.last_mut().unwrap().1
        }
    }

    loop {
        let (name, new_offset) = read_ue3(data, offset)?;
        offset = new_offset;
        if name == "None" {
            break;
        }

        let tag_offset = offset;
        let (tag, new_offset) = read_ue3(data, offset)?;
        offset = new_offset;

        let (val, vidx) = if !is_type_tag(&tag) {
            // no type tag, recurse: either a value-type struct or a class-in-array
            offset = tag_offset;
            let (mut inner, new_offset) = parse_property_stream(data, offset)?;
            offset = new_offset;
            if let Some(obj) = inner.as_object_mut() {
                obj.insert("__type".to_string(), json!(name));
            }
            (inner, 0i64)
        } else {
            let vlen = read_i32(data, offset)?;
            offset += 4;
            let vidx = read_i32(data, offset)? as i64;
            offset += 4;
            let (val, new_offset) = parse_value(data, offset, &tag, vlen)?;
            offset = new_offset;
            (val, vidx)
        };

        let in_fixed = fixed.iter().any(|(n, _)| n == &name);
        if vidx != 0 {
            fixed_entry(&mut fixed, &name).insert(vidx, val);
        } else if props.contains_key(&name) || in_fixed {
            fixed_entry(&mut fixed, &name).insert(0, val);
        } else {
            props.insert(name, val);
        }
    }

    for (name, mut idxmap) in fixed {
        if let Some(existing) = props.remove(&name) {
            idxmap.insert(0, existing);
        }
        let list: Vec<Value> = idxmap.into_values().collect();
        props.insert(name, Value::Array(list));
    }

    Ok((Value::Object(props), offset))
}

// Value parsers

fn parse_value(data: &[u8], offset: usize, tag: &str, vlen: i32) -> PResult<(Value, usize)> {
    match tag {
        "BoolProperty" => {
            let b = *data
                .get(offset)
                .ok_or_else(|| format!("bool out of range at {offset}"))?;
            Ok((json!(b != 0), offset + 1))
        }
        "IntProperty" => Ok((json!(read_i32(data, offset)?), offset + 4)),
        "QWordProperty" => Ok((json!(read_u64(data, offset)?), offset + 8)),
        "FloatProperty" => Ok((json!(round6(read_f32(data, offset)? as f64)), offset + 4)),
        "StrProperty" | "NameProperty" => {
            let (s, off) = read_ue3(data, offset)?;
            Ok((json!(s), off))
        }
        "ByteProperty" => {
            let (tn, off) = read_ue3(data, offset)?;
            if tn == "None" {
                let b = *data
                    .get(off)
                    .ok_or_else(|| format!("byte out of range at {off}"))?;
                Ok((json!(b), off + 1))
            } else {
                let (val, off) = read_ue3(data, off)?;
                Ok((json!(val), off))
            }
        }
        "ObjectProperty" => Ok((json!(read_i32(data, offset)?), offset + 4)),
        "StructProperty" => parse_struct(data, offset),
        "ArrayProperty" => parse_array(data, offset, vlen),
        _ => Err(format!("Unknown tag {tag:?} at offset {offset}")),
    }
}

fn parse_struct(data: &[u8], offset: usize) -> PResult<(Value, usize)> {
    let (tn, offset) = read_ue3(data, offset)?;

    // ISpecialSerialized: fixed binary layout
    match tn.as_str() {
        "Vector" => {
            let x = read_f32(data, offset)? as f64;
            let y = read_f32(data, offset + 4)? as f64;
            let z = read_f32(data, offset + 8)? as f64;
            return Ok((
                json!({"x": round6(x), "y": round6(y), "z": round6(z), "__type": "Vector"}),
                offset + 12,
            ));
        }
        "Rotator" => {
            let p = read_f32(data, offset)? as f64;
            let y = read_f32(data, offset + 4)? as f64;
            let r = read_f32(data, offset + 8)? as f64;
            return Ok((
                json!({"pitch": round6(p), "yaw": round6(y), "roll": round6(r), "__type": "Rotator"}),
                offset + 12,
            ));
        }
        "Guid" => {
            let a = read_u32(data, offset)?;
            let b = read_u32(data, offset + 4)?;
            let c = read_u32(data, offset + 8)?;
            let d = read_u32(data, offset + 12)?;
            return Ok((
                json!(format!("{a:08X}-{b:08X}-{c:08X}-{d:08X}")),
                offset + 16,
            ));
        }
        _ => {}
    }

    // Class in an array: TypeName + 0xFFFFFFFF marker + property stream
    let marker = read_u32(data, offset)?;
    if marker == OBJHEADER {
        let (mut props, off) = parse_property_stream(data, offset + 4)?;
        if let Some(obj) = props.as_object_mut() {
            obj.insert("__type".to_string(), json!(tn));
        }
        return Ok((props, off));
    }

    // Value-type struct: property stream follows immediately after type name
    let (mut props, off) = parse_property_stream(data, offset)?;
    if let Some(obj) = props.as_object_mut() {
        obj.insert("__type".to_string(), json!(tn));
    }
    Ok((props, off))
}

fn parse_array(data: &[u8], offset: usize, vlen: i32) -> PResult<(Value, usize)> {
    let count = read_i32(data, offset)?;
    let mut offset = offset + 4;
    if count <= 0 {
        return Ok((json!([]), offset));
    }
    let count = count as usize;

    // heuristic: uniform arrays have a known per-element size
    let payload = vlen as i64 - 4;
    let elem_hint = if payload > 0 {
        payload / count as i64
    } else {
        0
    };
    let mut elems: Vec<Value> = Vec::with_capacity(count);

    match elem_hint {
        4 => {
            for _ in 0..count {
                elems.push(json!(read_i32(data, offset)?));
                offset += 4;
            }
            return Ok((Value::Array(elems), offset));
        }
        1 => {
            for _ in 0..count {
                let b = *data
                    .get(offset)
                    .ok_or_else(|| format!("byte array out of range at {offset}"))?;
                elems.push(json!(b));
                offset += 1;
            }
            return Ok((Value::Array(elems), offset));
        }
        8 => {
            for _ in 0..count {
                elems.push(json!(read_u64(data, offset)?));
                offset += 8;
            }
            return Ok((Value::Array(elems), offset));
        }
        _ => {}
    }

    // Fallback: sniff each element.
    for _ in 0..count {
        let (elem, new_offset) = parse_array_elem(data, offset)?;
        offset = new_offset;
        elems.push(elem);
    }
    Ok((Value::Array(elems), offset))
}

/// sniff one array element's type (arrays carry no type tags)
fn parse_array_elem(data: &[u8], offset: usize) -> PResult<(Value, usize)> {
    let (s, after1) = match read_ue3(data, offset) {
        Ok(v) => v,
        Err(_) => return Ok((json!(read_i32(data, offset)?), offset + 4)),
    };

    if s == "None" {
        return match read_ue3(data, after1) {
            Ok(_) => Ok((json!({}), after1)), // empty struct
            Err(_) => {
                let b = *data
                    .get(after1)
                    .ok_or_else(|| format!("byte out of range at {after1}"))?;
                Ok((json!(b), after1 + 1))
            }
        };
    }

    if s.is_empty() {
        return Ok((json!(s), after1));
    }

    // Class element: TypeName + 0xFFFFFFFF + propstream
    if after1 + 4 <= data.len() && read_u32(data, after1)? == OBJHEADER {
        let (mut props, off) = parse_property_stream(data, after1 + 4)?;
        if let Some(obj) = props.as_object_mut() {
            obj.insert("__type".to_string(), json!(s));
        }
        return Ok((props, off));
    }

    // Sniff: is the next token a property name or a type tag?
    let sniffed: PResult<Option<(Value, usize)>> = (|| {
        let (maybe_prop, after2) = read_ue3(data, after1)?;

        if maybe_prop == "StructProperty" {
            let mut content = after2 + 8;
            if content + 4 <= data.len() && read_u32(data, content)? == OBJHEADER {
                content += 4;
            }
            let (mut props, off) = parse_property_stream(data, content)?;
            if let Some(obj) = props.as_object_mut() {
                obj.insert("__type".to_string(), json!(s));
            }
            return Ok(Some((props, off)));
        }

        if is_type_tag(&maybe_prop) {
            let (props, off) = parse_property_stream(data, offset)?;
            return Ok(Some((props, off)));
        }

        let (maybe_tag, _) = read_ue3(data, after2)?;
        if s.contains('.') && is_type_tag(&maybe_tag) {
            let (mut props, off) = parse_property_stream(data, after1)?;
            if let Some(obj) = props.as_object_mut() {
                obj.insert("__type".to_string(), json!(s));
            }
            return Ok(Some((props, off)));
        }
        Ok(None)
    })();

    if let Ok(Some(result)) = sniffed {
        return Ok(result);
    }

    Ok((json!(s), after1))
}
