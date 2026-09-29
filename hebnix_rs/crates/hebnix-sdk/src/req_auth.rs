use std::time::{SystemTime, UNIX_EPOCH};

use hmac::{Hmac, Mac};
use md5::{Digest, Md5};
use sha1::Sha1;

include!(concat!(env!("OUT_DIR"), "/req_key.rs"));

fn decode_base32(value: &str) -> Result<Vec<u8>, String> {
    let mut output = Vec::new();
    let mut bits = 0u32;
    let mut count = 0u8;
    for ch in value.bytes().filter(|ch| !ch.is_ascii_whitespace() && *ch != b'-') {
        if ch == b'=' {
            break;
        }
        let digit = match ch.to_ascii_uppercase() {
            b'A'..=b'Z' => ch.to_ascii_uppercase() - b'A',
            b'2'..=b'7' => ch - b'2' + 26,
            _ => return Err("request key must be Base32 encoded".into()),
        };
        bits = (bits << 5) | u32::from(digit);
        count += 5;
        if count >= 8 {
            count -= 8;
            output.push((bits >> count) as u8);
            bits &= (1 << count) - 1;
        }
    }
    if output.is_empty() {
        return Err("request key is missing".into());
    }
    Ok(output)
}

fn token_at(key: &str, unix_seconds: u64) -> Result<String, String> {
    let secret = decode_base32(key)?;
    let counter = (unix_seconds / 30).to_be_bytes();
    let mut mac = Hmac::<Sha1>::new_from_slice(&secret).map_err(|_| "invalid request key")?;
    mac.update(&counter);
    let result = mac.finalize().into_bytes();
    let offset = usize::from(result[19] & 0x0f);
    let number = u32::from_be_bytes(result[offset..offset + 4].try_into().unwrap()) & 0x7fff_ffff;
    let code = format!("{:08}", number % 100_000_000);
    let digest = Md5::digest(code.as_bytes());
    Ok(format!("{digest:x}"))
}

pub fn app_token() -> Result<String, String> {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "system clock is before Unix epoch")?
        .as_secs();
    token_at(KEY, seconds)
}
