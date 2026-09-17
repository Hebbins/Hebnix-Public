use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};

// Try Rocket League's default package key before the embedded catalog.
// file order made large packages such as Startup.upk get AES-decrypted almost
// one thousand times before the correct key was reached.
pub const DEFAULT_UPK_KEY: [u8; 32] = [
    199, 223, 107, 19, 37, 42, 204, 113, 71, 187, 81, 201, 138, 215, 227, 75, 127, 229, 0, 183,
    127, 165, 250, 178, 147, 226, 242, 78, 107, 23, 231, 121,
];

pub fn embedded() -> Result<Vec<(usize, [u8; 32])>, String> {
    parse(
        include_str!("../../assets/upk_keys.txt"),
        "embedded UPK key catalog",
    )
}

pub fn parse(text: &str, source: &str) -> Result<Vec<(usize, [u8; 32])>, String> {
    // Line 0 identifies the built-in default rather than a catalog line.
    let mut keys = vec![(0, DEFAULT_UPK_KEY)];
    for (index, line) in text.lines().enumerate() {
        let value = line.trim();
        if value.is_empty() || value.starts_with('#') {
            continue;
        }
        let Ok(decoded) = BASE64_STANDARD.decode(value) else {
            continue;
        };
        let Ok(key) = <[u8; 32]>::try_from(decoded.as_slice()) else {
            continue;
        };
        if !keys.iter().any(|(_, existing)| existing == &key) {
            keys.push((index + 1, key));
        }
    }
    if keys.len() == 1
        && text
            .lines()
            .all(|line| line.trim().is_empty() || line.trim().starts_with('#'))
    {
        return Err(format!("{source} contains no valid AES-256 keys"));
    }
    Ok(keys)
}
