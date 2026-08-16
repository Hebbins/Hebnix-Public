//! ue3 binary property-stream serializer (experimental round-trip). turns
//! serde_json::Value trees back into RL's tagged-property format.

use serde_json::Value;

use crate::save_file::crypto::OBJHEADER;

/// encode a string as ue3 length-prefixed utf-8
pub fn write_ue3(s: &str) -> Vec<u8> {
    let encoded = s.as_bytes();
    let mut out = Vec::with_capacity(encoded.len() + 5);
    out.extend_from_slice(&((encoded.len() as i32 + 1).to_le_bytes()));
    out.extend_from_slice(encoded);
    out.push(0);
    out
}

/// serialize a property object into the ue3 tagged-property format
pub fn serialize_property_stream(props: &Value) -> Vec<u8> {
    let mut buf: Vec<u8> = Vec::new();
    let Some(obj) = props.as_object() else {
        buf.extend_from_slice(&write_ue3("None"));
        return buf;
    };

    let scalars = obj.iter().filter(|(_, v)| !v.is_array());
    let arrays = obj.iter().filter(|(_, v)| v.is_array());

    for (name, val) in scalars {
        buf.extend_from_slice(&write_ue3(name));
        let (tag, body) = serialize_value(val, false);
        buf.extend_from_slice(&write_ue3(tag));
        buf.extend_from_slice(&(body.len() as i32).to_le_bytes());
        buf.extend_from_slice(&0i32.to_le_bytes()); // vidx
        buf.extend_from_slice(&body);
    }

    for (name, arr) in arrays {
        let items = arr.as_array().unwrap();
        let mut payload: Vec<u8> = Vec::new();
        payload.extend_from_slice(&(items.len() as i32).to_le_bytes());
        for elem in items {
            let (_, ebody) = serialize_value(elem, true);
            payload.extend_from_slice(&ebody);
        }

        buf.extend_from_slice(&write_ue3(name));
        buf.extend_from_slice(&write_ue3("ArrayProperty"));
        buf.extend_from_slice(&(payload.len() as i32).to_le_bytes());
        buf.extend_from_slice(&0i32.to_le_bytes()); // vidx
        buf.extend_from_slice(&payload);
    }

    buf.extend_from_slice(&write_ue3("None"));
    buf
}

fn serialize_value(val: &Value, is_array_elem: bool) -> (&'static str, Vec<u8>) {
    match val {
        Value::Bool(b) => ("BoolProperty", vec![u8::from(*b)]),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                if !(i32::MIN as i64..=i32::MAX as i64).contains(&i) {
                    ("QWordProperty", (i as u64).to_le_bytes().to_vec())
                } else {
                    ("IntProperty", (i as i32).to_le_bytes().to_vec())
                }
            } else if let Some(u) = n.as_u64() {
                if u > i32::MAX as u64 {
                    ("QWordProperty", u.to_le_bytes().to_vec())
                } else {
                    ("IntProperty", (u as i32).to_le_bytes().to_vec())
                }
            } else {
                let f = n.as_f64().unwrap_or(0.0) as f32;
                ("FloatProperty", f.to_le_bytes().to_vec())
            }
        }
        Value::String(s) => ("StrProperty", write_ue3(s)),
        Value::Object(_) => serialize_struct(val, is_array_elem),
        Value::Array(items) => {
            let mut payload: Vec<u8> = Vec::new();
            payload.extend_from_slice(&(items.len() as i32).to_le_bytes());
            for elem in items {
                let (_, ebody) = serialize_value(elem, true);
                payload.extend_from_slice(&ebody);
            }
            ("ArrayProperty", payload)
        }
        Value::Null => ("IntProperty", 0i32.to_le_bytes().to_vec()),
    }
}

fn serialize_struct(d: &Value, is_array_elem: bool) -> (&'static str, Vec<u8>) {
    let obj = d.as_object().unwrap();
    let tn = obj
        .get("__type")
        .and_then(|v| v.as_str())
        .unwrap_or("Unknown")
        .to_string();

    let props: serde_json::Map<String, Value> = obj
        .iter()
        .filter(|(k, _)| *k != "__type")
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    let props = Value::Object(props);

    let body: Vec<u8> = match tn.as_str() {
        "Vector" | "Rotator" => {
            let get = |a: &str, b: &str| -> f32 {
                props
                    .get(a)
                    .or_else(|| props.get(b))
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0) as f32
            };
            let x = get("x", "pitch");
            let y = get("y", "yaw");
            let z = get("z", "roll");
            let mut b = Vec::with_capacity(12);
            b.extend_from_slice(&x.to_le_bytes());
            b.extend_from_slice(&y.to_le_bytes());
            b.extend_from_slice(&z.to_le_bytes());
            b
        }
        "Guid" => vec![0u8; 16],
        "Unknown" => {
            return ("StructProperty", serialize_property_stream(&props));
        }
        _ if tn.contains('.') && is_array_elem => {
            let mut b = Vec::new();
            b.extend_from_slice(&OBJHEADER.to_le_bytes());
            b.extend_from_slice(&serialize_property_stream(&props));
            b
        }
        _ => serialize_property_stream(&props),
    };

    let mut out = write_ue3(&tn);
    out.extend_from_slice(&body);
    ("StructProperty", out)
}
