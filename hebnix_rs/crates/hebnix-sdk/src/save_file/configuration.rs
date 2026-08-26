use crate::process::is_rocket_league_running;
use crate::save_file::binary_parser::read_ue3;
use crate::save_file::crypto::{SaveError, aes_decrypt, aes_encrypt, crc32, is_type_tag};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const CATEGORIES: &[(&str, &str)] = &[
    ("TAGame.ProfileGamepadSave_TA", "Controller"),
    ("TAGame.ProfileCameraSave_TA", "Camera"),
    ("TAGame.GameplaySettingsSave_TA", "Gameplay / Interface"),
    ("TAGame.SoundSettingsSave_TA", "Audio"),
    ("TAGame.VideoSettingsSavePC_TA", "Video"),
    ("TAGame.VideoSettingsSave_TA", "Video"),
    ("TAGame.MatchmakingSettingsSave_TA", "Matchmaking"),
];

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ConfigurationEditorKind {
    ReadOnly,
    Boolean,
    Integer,
    Float,
    Byte,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigurationValue {
    pub id: String,
    pub category: String,
    pub name: String,
    pub value: String,
    pub editor_kind: ConfigurationEditorKind,
    #[serde(skip)]
    payload_offset: usize,
}

fn read_i32(data: &[u8], offset: usize) -> Result<i32, SaveError> {
    data.get(offset..offset + 4)
        .map(|v| i32::from_le_bytes(v.try_into().unwrap()))
        .ok_or_else(|| SaveError::Parse(format!("i32 out of range at {offset}")))
}
fn read_u32(data: &[u8], offset: usize) -> Result<u32, SaveError> {
    data.get(offset..offset + 4)
        .map(|v| u32::from_le_bytes(v.try_into().unwrap()))
        .ok_or_else(|| SaveError::Parse(format!("u32 out of range at {offset}")))
}
fn read_u64(data: &[u8], offset: usize) -> Result<u64, SaveError> {
    data.get(offset..offset + 8)
        .map(|v| u64::from_le_bytes(v.try_into().unwrap()))
        .ok_or_else(|| SaveError::Parse(format!("u64 out of range at {offset}")))
}
fn check(data: &[u8], offset: usize, length: usize) -> Result<(), SaveError> {
    if offset > data.len().saturating_sub(length) {
        return Err(SaveError::Parse("unexpected end of save file".into()));
    }
    Ok(())
}
fn friendly(value: &str) -> String {
    let bytes = value.as_bytes();
    let value = if bytes.len() > 1 && bytes[0] == b'b' && bytes[1].is_ascii_uppercase() {
        &value[1..]
    } else {
        value
    };
    let mut output = String::new();
    let mut previous_upper = true;
    for ch in value.chars() {
        if !output.is_empty() && ch.is_uppercase() && !previous_upper {
            output.push(' ');
        }
        previous_upper = ch.is_uppercase();
        output.push(ch);
    }
    output
}
fn category(type_name: &str) -> Option<&'static str> {
    CATEGORIES
        .iter()
        .find_map(|(name, category)| (*name == type_name).then_some(*category))
}
fn add_value(
    output: &mut Vec<ConfigurationValue>,
    object: usize,
    category: &str,
    name: String,
    value: String,
    kind: ConfigurationEditorKind,
    offset: usize,
) {
    output.push(ConfigurationValue {
        id: format!("{object}:{offset}"),
        category: category.into(),
        name,
        value,
        editor_kind: kind,
        payload_offset: offset,
    });
}
fn skip_value(data: &[u8], offset: usize, tag: &str, length: usize) -> Result<usize, SaveError> {
    match tag {
        "BoolProperty" => Ok(offset + 1),
        "IntProperty" | "ObjectProperty" | "FloatProperty" => Ok(offset + 4),
        "QWordProperty" => Ok(offset + 8),
        "StrProperty" | "NameProperty" => read_ue3(data, offset)
            .map(|(_, off)| off)
            .map_err(SaveError::Parse),
        "ByteProperty" => {
            let (enum_name, next) = read_ue3(data, offset).map_err(SaveError::Parse)?;
            if enum_name == "None" {
                Ok(next + 1)
            } else {
                read_ue3(data, next)
                    .map(|(_, off)| off)
                    .map_err(SaveError::Parse)
            }
        }
        _ => Ok(offset + length),
    }
}
fn parse_object(
    data: &[u8],
    mut offset: usize,
    end: usize,
    object: usize,
    category: &str,
    output: &mut Vec<ConfigurationValue>,
) -> Result<(), SaveError> {
    while offset < end {
        let (name, next) = read_ue3(data, offset).map_err(SaveError::Parse)?;
        offset = next;
        if name == "None" {
            break;
        }
        let (tag, next) = read_ue3(data, offset).map_err(SaveError::Parse)?;
        offset = next;
        if !is_type_tag(&tag) {
            return Err(SaveError::Parse(format!(
                "unsupported nested property {name}"
            )));
        }
        let length = read_i32(data, offset)?;
        let array_index = read_i32(data, offset + 4)?;
        offset += 8;
        if length < 0 {
            return Err(SaveError::Parse("negative property length".into()));
        }
        let value_offset = offset;
        let label = if array_index == 0 {
            friendly(&name)
        } else {
            format!("{} [{}]", friendly(&name), array_index)
        };
        match tag.as_str() {
            "BoolProperty" => {
                check(data, offset, 1)?;
                add_value(
                    output,
                    object,
                    category,
                    label,
                    (data[offset] != 0).to_string(),
                    ConfigurationEditorKind::Boolean,
                    value_offset,
                );
            }
            "IntProperty" => add_value(
                output,
                object,
                category,
                label,
                read_i32(data, offset)?.to_string(),
                ConfigurationEditorKind::Integer,
                value_offset,
            ),
            "FloatProperty" => {
                check(data, offset, 4)?;
                add_value(
                    output,
                    object,
                    category,
                    label,
                    f32::from_le_bytes(data[offset..offset + 4].try_into().unwrap()).to_string(),
                    ConfigurationEditorKind::Float,
                    value_offset,
                );
            }
            "StructProperty" => {
                let (struct_type, struct_offset) =
                    read_ue3(data, offset).map_err(SaveError::Parse)?;
                let names: Option<[&str; 3]> = match struct_type.as_str() {
                    "Vector" => Some(["X", "Y", "Z"]),
                    "Rotator" => Some(["Pitch", "Yaw", "Roll"]),
                    _ => None,
                };
                if let Some(names) = names {
                    for (index, name) in names.iter().enumerate() {
                        let field_offset = struct_offset + index * 4;
                        check(data, field_offset, 4)?;
                        add_value(
                            output,
                            object,
                            category,
                            format!("{label} / {name}"),
                            f32::from_le_bytes(
                                data[field_offset..field_offset + 4].try_into().unwrap(),
                            )
                            .to_string(),
                            ConfigurationEditorKind::Float,
                            field_offset,
                        );
                    }
                }
            }
            "QWordProperty" => add_value(
                output,
                object,
                category,
                label,
                read_u64(data, offset)?.to_string(),
                ConfigurationEditorKind::ReadOnly,
                value_offset,
            ),
            "StrProperty" | "NameProperty" => {
                let (value, _) = read_ue3(data, offset).map_err(SaveError::Parse)?;
                add_value(
                    output,
                    object,
                    category,
                    label,
                    value,
                    ConfigurationEditorKind::ReadOnly,
                    value_offset,
                );
            }
            "ObjectProperty" => add_value(
                output,
                object,
                category,
                label,
                read_i32(data, offset)?.to_string(),
                ConfigurationEditorKind::ReadOnly,
                value_offset,
            ),
            "ByteProperty" => {
                let (enum_name, byte_offset) = read_ue3(data, offset).map_err(SaveError::Parse)?;
                if enum_name == "None" {
                    check(data, byte_offset, 1)?;
                    add_value(
                        output,
                        object,
                        category,
                        label,
                        data[byte_offset].to_string(),
                        ConfigurationEditorKind::Byte,
                        byte_offset,
                    );
                } else {
                    let (value, _) = read_ue3(data, byte_offset).map_err(SaveError::Parse)?;
                    add_value(
                        output,
                        object,
                        category,
                        label,
                        value,
                        ConfigurationEditorKind::ReadOnly,
                        value_offset,
                    );
                }
            }
            _ => {}
        }
        offset = skip_value(data, offset, &tag, length as usize)?;
    }
    Ok(())
}
fn decrypted_save(path: &Path) -> Result<(Vec<u8>, Vec<u8>), SaveError> {
    let raw = std::fs::read(path)?;
    let length = read_u32(&raw, 0)? as usize;
    check(&raw, 8, length)?;
    if length == 0 || length % 16 != 0 {
        return Err(SaveError::Parse("invalid encrypted payload".into()));
    }
    let decrypted = aes_decrypt(&raw[8..8 + length]);
    Ok((raw, decrypted))
}
fn append_raw_values(
    output: &mut Vec<ConfigurationValue>,
    existing: &mut std::collections::HashSet<(String, String)>,
    object: usize,
    category: &str,
    prefix: String,
    value: &serde_json::Value,
) {
    match value {
        serde_json::Value::Object(map) => {
            for (name, child) in map {
                if name.starts_with("__") || name.to_ascii_lowercase().contains("gamepadbinding") {
                    continue;
                }
                let path = if prefix.is_empty() {
                    friendly(name)
                } else {
                    format!("{prefix} / {}", friendly(name))
                };
                append_raw_values(output, existing, object, category, path, child);
            }
        }
        serde_json::Value::Array(items) => {
            if items
                .iter()
                .all(|item| !item.is_object() && !item.is_array())
            {
                let text = items
                    .iter()
                    .map(serde_json::Value::to_string)
                    .collect::<Vec<_>>()
                    .join(", ");
                if existing.insert((category.to_string(), prefix.clone())) {
                    add_value(
                        output,
                        object,
                        category,
                        prefix,
                        text,
                        ConfigurationEditorKind::ReadOnly,
                        0,
                    );
                }
            } else {
                for (index, child) in items.iter().enumerate() {
                    append_raw_values(
                        output,
                        existing,
                        object,
                        category,
                        format!("{prefix} [{}]", index + 1),
                        child,
                    );
                }
            }
        }
        serde_json::Value::Null => {}
        _ => {
            if existing.insert((category.to_string(), prefix.clone())) {
                let text = value
                    .as_str()
                    .map(str::to_string)
                    .unwrap_or_else(|| value.to_string());
                add_value(
                    output,
                    object,
                    category,
                    prefix,
                    text,
                    ConfigurationEditorKind::ReadOnly,
                    0,
                );
            }
        }
    }
}

fn friendly_action(value: &str) -> String {
    match value {
        "Roll" => "Air Roll".into(),
        "RollLeft" => "Air Roll Left".into(),
        "RollRight" => "Air Roll Right".into(),
        "Handbrake" => "Powerslide".into(),
        "ToggleScoreboard" => "Scoreboard".into(),
        "FocusOnBall" | "SecondaryCamera" => "Ball Cam".into(),
        "ToggleRoll" => "Air Roll".into(),
        "RearView" | "RearCamera" => "Rear View".into(),
        "UsePickup" => "Use Item".into(),
        "ToggleMidGameMenu" => "Pause Menu".into(),
        "ThrottleForward" => "Accelerate".into(),
        "ThrottleReverse" => "Reverse".into(),
        "ChatPreset1" => "Quick Chat Up".into(),
        "ChatPreset2" => "Quick Chat Left".into(),
        "ChatPreset3" => "Quick Chat Right".into(),
        "ChatPreset4" => "Quick Chat Down".into(),
        _ => friendly(value),
    }
}

fn friendly_controller_key(value: &str) -> String {
    let value = value.strip_prefix("XboxTypeS_").unwrap_or(value);
    match value {
        "LeftShoulder" => "Left Bumper".into(),
        "RightShoulder" => "Right Bumper".into(),
        "LeftTriggerAxis" => "Left Trigger".into(),
        "RightTriggerAxis" => "Right Trigger".into(),
        "LeftThumbstick" => "Left Stick Click".into(),
        "RightThumbstick" => "Right Stick Click".into(),
        "Start" => "Menu".into(),
        "Back" => "View".into(),
        "LeftX" => "Left Stick X".into(),
        "LeftY" => "Left Stick Y".into(),
        "RightX" => "Right Stick X".into(),
        "RightY" => "Right Stick Y".into(),
        "DPad_Up" => "D-Pad Up".into(),
        "DPad_Down" => "D-Pad Down".into(),
        "DPad_Left" => "D-Pad Left".into(),
        "DPad_Right" => "D-Pad Right".into(),
        "None" | "" => "Unbound".into(),
        _ => friendly(value),
    }
}

fn binding_text(
    action: &str,
    key: &str,
    axis: Option<&str>,
    press: Option<&str>,
) -> (String, String) {
    let mut details = Vec::new();
    if let Some(axis) = axis.filter(|axis| *axis != "AxisSign_None") {
        details.push(friendly(axis.strip_prefix("AxisSign_").unwrap_or(axis)));
    }
    if let Some(press) = press.filter(|press| *press != "BPT_Normal") {
        details.push(friendly(press.strip_prefix("BPT_").unwrap_or(press)));
    }
    let mut key = friendly_controller_key(key);
    if !details.is_empty() {
        key.push_str(&format!(" ({})", details.join(", ")));
    }
    (friendly_action(action), key)
}

fn append_controller_bindings(path: &Path, output: &mut Vec<ConfigurationValue>) {
    let mut bindings = std::collections::BTreeMap::new();
    if let Some(tagame) = path.parent().and_then(Path::parent).and_then(Path::parent) {
        let input = tagame.join("Config").join("TAInput.ini");
        if let Ok(contents) = std::fs::read_to_string(input) {
            let action_re =
                regex::Regex::new(r#"Action\s*=\s*"([^"]*)""#).expect("valid action regex");
            let key_re = regex::Regex::new(r#"Key\s*=\s*"([^"]*)""#).expect("valid key regex");
            let axis_re =
                regex::Regex::new(r"AxisSign\s*=\s*([^,\s\)]+)").expect("valid axis regex");
            let press_re =
                regex::Regex::new(r"PressType\s*=\s*([^,\s\)]+)").expect("valid press regex");
            let mut default_preset = false;
            for line in contents.lines() {
                let line = line.trim();
                if line.starts_with('[') {
                    default_preset = line.eq_ignore_ascii_case("[ProjectX.ControlPreset_X]");
                    continue;
                }
                if !default_preset
                    || !line
                        .get(.."GamepadBindings=".len())
                        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("GamepadBindings="))
                {
                    continue;
                }
                let Some(action) = action_re
                    .captures(line)
                    .and_then(|capture| capture.get(1))
                    .map(|capture| capture.as_str())
                else {
                    continue;
                };
                let key = key_re
                    .captures(line)
                    .and_then(|capture| capture.get(1))
                    .map(|capture| capture.as_str())
                    .unwrap_or("None");
                let axis = axis_re
                    .captures(line)
                    .and_then(|capture| capture.get(1))
                    .map(|capture| capture.as_str());
                let press = press_re
                    .captures(line)
                    .and_then(|capture| capture.get(1))
                    .map(|capture| capture.as_str());
                let (action, display) = binding_text(action, key, axis, press);
                bindings.insert(action, display);
            }
        }
    }
    if let Ok(save) = crate::save_file::load(path, false) {
        if let Some(gamepad) = save.gamepad_bindings() {
            for binding in gamepad.raw_bindings.as_array().into_iter().flatten() {
                let Some(action) = binding.get("Action").and_then(serde_json::Value::as_str) else {
                    continue;
                };
                let key = binding
                    .get("Key")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("None");
                let axis = binding.get("AxisSign").and_then(serde_json::Value::as_str);
                let press = binding.get("PressType").and_then(serde_json::Value::as_str);
                let (action, display) = binding_text(action, key, axis, press);
                bindings.insert(action, display);
            }
        }
    }
    for (action, value) in bindings {
        output.push(ConfigurationValue {
            id: format!("binding:{action}"),
            category: "Controller Bindings".into(),
            name: action,
            value,
            editor_kind: ConfigurationEditorKind::ReadOnly,
            payload_offset: 0,
        });
    }
}

pub fn list_configuration(path: &Path) -> Result<Vec<ConfigurationValue>, SaveError> {
    let (_, data) = decrypted_save(path)?;
    let save_length = read_i32(&data, 20)?;
    if save_length < 4 {
        return Err(SaveError::Parse("invalid save data length".into()));
    }
    let payload_start = 24;
    let payload_end = payload_start + save_length as usize - 4;
    check(&data, payload_start, save_length as usize - 4)?;
    let count = read_i32(&data, payload_end)?;
    if !(0..=100_000).contains(&count) {
        return Err(SaveError::Parse("invalid object table".into()));
    }
    let mut offset = payload_end + 4;
    let mut objects = Vec::new();
    for _ in 0..count as usize {
        let (type_name, next) = read_ue3(&data, offset).map_err(SaveError::Parse)?;
        offset = next;
        let position = read_u32(&data, offset)? as usize;
        offset += 8;
        objects.push((type_name, position));
    }
    let payload = &data[payload_start..payload_end];
    let mut values = Vec::new();
    for (index, (type_name, start)) in objects.iter().enumerate() {
        let Some(category) = category(type_name) else {
            continue;
        };
        let end = objects
            .get(index + 1)
            .map(|(_, pos)| pos.saturating_sub(4))
            .unwrap_or(payload.len());
        if *start < end && end <= payload.len() {
            let _ = parse_object(payload, *start, end, index, category, &mut values);
        }
    }
    let mut existing: std::collections::HashSet<(String, String)> = values
        .iter()
        .map(|value| (value.category.clone(), value.name.clone()))
        .collect();
    if let Ok(save) = crate::save_file::load(path, false) {
        for (index, parsed) in save.parsed_objects().iter().enumerate() {
            if let Some(category) = category(&parsed.type_name) {
                append_raw_values(
                    &mut values,
                    &mut existing,
                    index,
                    category,
                    String::new(),
                    &parsed.properties,
                );
            }
        }
    }
    values.retain(|value| {
        !(value.category == "Controller" && value.name.starts_with("Gamepad Bindings"))
    });
    append_controller_bindings(path, &mut values);
    values.sort_by(|a, b| {
        a.category
            .cmp(&b.category)
            .then_with(|| a.name.cmp(&b.name))
    });
    Ok(values)
}
pub fn validate_configuration(path: &Path) -> Result<(), SaveError> {
    let (raw, _) = decrypted_save(path)?;
    let length = read_u32(&raw, 0)? as usize;
    if read_u32(&raw, 4)? != crc32(&raw[8..8 + length]) {
        return Err(SaveError::Parse(
            "the save file failed its Rocket League CRC check".into(),
        ));
    }
    let _ = list_configuration(path)?;
    Ok(())
}
fn backup_path(path: &Path, suffix: &str) -> PathBuf {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    PathBuf::from(format!("{}.{}-{stamp}.bak", path.to_string_lossy(), suffix))
}
pub fn update_configuration(path: &Path, id: &str, input: &str) -> Result<PathBuf, SaveError> {
    if is_rocket_league_running() {
        return Err(SaveError::Parse(
            "close Rocket League before changing a save file".into(),
        ));
    }
    let setting = list_configuration(path)?
        .into_iter()
        .find(|entry| entry.id == id)
        .ok_or_else(|| SaveError::Parse("save setting was not found".into()))?;
    let (mut raw, mut data) = decrypted_save(path)?;
    let offset = 24 + setting.payload_offset;
    match setting.editor_kind {
        ConfigurationEditorKind::Boolean => {
            data[offset] = input
                .parse::<bool>()
                .map_err(|_| SaveError::Parse("enter true or false".into()))?
                as u8
        }
        ConfigurationEditorKind::Integer => data[offset..offset + 4].copy_from_slice(
            &input
                .parse::<i32>()
                .map_err(|_| SaveError::Parse("enter a whole number".into()))?
                .to_le_bytes(),
        ),
        ConfigurationEditorKind::Float => {
            let value = input
                .parse::<f32>()
                .map_err(|_| SaveError::Parse("enter a valid number".into()))?;
            if !value.is_finite() {
                return Err(SaveError::Parse("enter a valid number".into()));
            }
            data[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        }
        ConfigurationEditorKind::Byte => {
            data[offset] = input
                .parse::<u8>()
                .map_err(|_| SaveError::Parse("enter a value from 0 to 255".into()))?
        }
        ConfigurationEditorKind::ReadOnly => {
            return Err(SaveError::Parse("this setting is read-only".into()));
        }
    }
    let encrypted = aes_encrypt(&data);
    if encrypted.len() != read_u32(&raw, 0)? as usize {
        return Err(SaveError::Parse(
            "edited save has an unexpected length".into(),
        ));
    }
    raw[4..8].copy_from_slice(&crc32(&encrypted).to_le_bytes());
    raw[8..8 + encrypted.len()].copy_from_slice(&encrypted);
    let temporary = path.with_extension("save.hebnix.tmp");
    std::fs::write(&temporary, raw)?;
    validate_configuration(&temporary)?;
    let backup = backup_path(path, "edit");
    std::fs::copy(path, &backup)?;
    std::fs::rename(&temporary, path)?;
    Ok(backup)
}
pub fn restore_configuration(path: &Path, backup: &Path) -> Result<PathBuf, SaveError> {
    if is_rocket_league_running() {
        return Err(SaveError::Parse(
            "close Rocket League before restoring a save file".into(),
        ));
    }
    if path == backup {
        return Err(SaveError::Parse(
            "choose a backup file rather than the active save".into(),
        ));
    }
    validate_configuration(backup)?;
    let safety = backup_path(path, "before-restore");
    std::fs::copy(path, &safety)?;
    let temporary = path.with_extension("save.hebnix.restore.tmp");
    std::fs::copy(backup, &temporary)?;
    std::fs::rename(&temporary, path)?;
    validate_configuration(path)?;
    Ok(safety)
}
