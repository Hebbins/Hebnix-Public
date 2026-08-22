//! aes encrypt/decrypt + crc32 for RL save files. ported from RLSaveViewer/RocketRP.

use aes::Aes256;
use aes::cipher::generic_array::GenericArray;
use aes::cipher::{BlockDecrypt, BlockEncrypt, KeyInit};

pub const AES_KEY: [u8; 32] = [
    0xD7, 0x8C, 0x32, 0x4A, 0x94, 0x42, 0x94, 0x3C, 0x6D, 0x65, 0xCE, 0x98, 0x81, 0x85, 0x4C, 0x41,
    0x68, 0x99, 0x22, 0x0C, 0xC7, 0xA1, 0x46, 0x40, 0x93, 0x9B, 0x96, 0x3C, 0x93, 0x2A, 0x6F, 0xAF,
];

pub const CRC_SEED: u32 = 0xEFCB_F201;
pub const OBJHEADER: u32 = 0xFFFF_FFFF;

pub const TYPE_TAGS: [&str; 10] = [
    "BoolProperty",
    "IntProperty",
    "QWordProperty",
    "FloatProperty",
    "StrProperty",
    "NameProperty",
    "ByteProperty",
    "ObjectProperty",
    "StructProperty",
    "ArrayProperty",
];

pub fn is_type_tag(s: &str) -> bool {
    TYPE_TAGS.contains(&s)
}

#[derive(Debug, thiserror::Error)]
pub enum SaveError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("parse error: {0}")]
    Parse(String),
}

// crc32, matches the C# Crc32.CalculateCRC RL uses

fn make_crc_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    for (i, entry) in table.iter_mut().enumerate() {
        let mut c = (i as u32) << 24;
        for _ in 0..8 {
            c = if c & 0x8000_0000 != 0 {
                (c << 1) ^ 0x04C1_1DB7
            } else {
                c << 1
            };
        }
        *entry = c;
    }
    table
}

pub fn crc32(data: &[u8]) -> u32 {
    static TABLE: std::sync::OnceLock<[u32; 256]> = std::sync::OnceLock::new();
    let table = TABLE.get_or_init(make_crc_table);

    let mut crc = !CRC_SEED;
    for &b in data {
        crc = (crc << 8) ^ table[((crc >> 24) ^ b as u32) as usize];
    }
    !crc
}

// aes ecb wrappers

/// decrypt with the hardcoded RL aes key (ecb)
pub fn aes_decrypt(data: &[u8]) -> Vec<u8> {
    let cipher = Aes256::new(GenericArray::from_slice(&AES_KEY));
    let mut out = data.to_vec();
    // ecb: each 16-byte block on its own. a trailing partial block is left as-is
    for block in out.chunks_exact_mut(16) {
        cipher.decrypt_block(GenericArray::from_mut_slice(block));
    }
    out
}

/// encrypt with the hardcoded RL aes key, null-pad to a 16-byte boundary
pub fn aes_encrypt(data: &[u8]) -> Vec<u8> {
    let cipher = Aes256::new(GenericArray::from_slice(&AES_KEY));
    let padded_len = (data.len() + 15) & !15;
    let mut out = data.to_vec();
    out.resize(padded_len, 0);
    for block in out.chunks_exact_mut(16) {
        cipher.encrypt_block(GenericArray::from_mut_slice(block));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aes_round_trip() {
        let plain = b"hello rocket league save data!!!"; // 32 bytes
        let encrypted = aes_encrypt(plain);
        assert_eq!(encrypted.len() % 16, 0);
        assert_ne!(&encrypted[..], &plain[..]);
        let decrypted = aes_decrypt(&encrypted);
        assert_eq!(&decrypted[..plain.len()], &plain[..]);
    }

    #[test]
    fn aes_pads_to_block_boundary() {
        let plain = b"short";
        let encrypted = aes_encrypt(plain);
        assert_eq!(encrypted.len(), 16);
        let decrypted = aes_decrypt(&encrypted);
        assert_eq!(&decrypted[..5], plain);
        assert_eq!(&decrypted[5..], &[0u8; 11]);
    }

    #[test]
    fn crc32_is_stable_and_sensitive() {
        let a = crc32(b"data");
        let b = crc32(b"data");
        let c = crc32(b"date");
        assert_eq!(a, b);
        assert_ne!(a, c);
    }
}
