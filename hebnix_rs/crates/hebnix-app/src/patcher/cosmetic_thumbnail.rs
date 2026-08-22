use crate::patch_core::upk::{self, UPK_MAGIC};
use image::{DynamicImage, ImageFormat, RgbaImage};
use std::io::Cursor;
use std::path::Path;

const TRAILER_SIZE: usize = 60;
const TEXTURE_CANDIDATES: [(usize, usize); 8] = [
    (2048, 2048),
    (1024, 1024),
    (512, 512),
    (512, 256),
    (256, 256),
    (256, 128),
    (128, 128),
    (64, 64),
];

fn bgra_png(bytes: &[u8], width: u32, height: u32) -> Result<Vec<u8>, String> {
    let needed = width as usize * height as usize * 4;
    if bytes.len() < needed {
        return Err("Thumbnail pixel buffer is truncated".into());
    }
    let mut rgba = bytes[..needed].to_vec();
    for pixel in rgba.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
    let image = RgbaImage::from_raw(width, height, rgba).ok_or("Invalid thumbnail dimensions")?;
    let mut output = Cursor::new(Vec::new());
    DynamicImage::ImageRgba8(image)
        .write_to(&mut output, ImageFormat::Png)
        .map_err(|error| error.to_string())?;
    Ok(output.into_inner())
}

fn generic_frame<'a>(bytes: &'a [u8], category: &str) -> Option<(&'a [u8], u32, u32)> {
    for (width, height) in TEXTURE_CANDIDATES {
        let pixels = width.checked_mul(height)?.checked_mul(4)?;
        let Some(start) = bytes.len().checked_sub(pixels + TRAILER_SIZE) else {
            continue;
        };
        if !(50..=10_000).contains(&start) {
            continue;
        }

        // Some item thumbnail packages contain two square Texture2D exports.
        // Their combined pixel count happens to equal one 2:1 candidate, so a
        // size-only decoder wraps both exports into a repeated 512x256 (or
        // 256x128) image. Player banners really are 2:1; every other catalog
        // uses the final square item thumbnail from this layout.
        if width == height * 2 && category != "banners" {
            let square_pixels = height.checked_mul(height)?.checked_mul(4)?;
            let square_start = bytes.len().checked_sub(square_pixels + TRAILER_SIZE)?;
            return Some((
                &bytes[square_start..square_start + square_pixels],
                height as u32,
                height as u32,
            ));
        }

        return Some((&bytes[start..start + pixels], width as u32, height as u32));
    }
    None
}

pub fn extract_png(path: &Path, category: &str) -> Result<Vec<u8>, String> {
    let raw = std::fs::read(path).map_err(|error| format!("{}: {error}", path.display()))?;
    let magic = UPK_MAGIC.to_le_bytes();
    let mut chunks = Vec::new();
    let mut offset = 4usize;
    while offset + 16 <= raw.len() {
        let Some(relative) = raw[offset..].windows(4).position(|window| window == magic) else {
            break;
        };
        let position = offset + relative;
        match upk::decomp_chunk_at(&raw, position) {
            Ok((bytes, _, end)) => {
                chunks.push(bytes);
                offset = end.max(position + 4);
            }
            Err(_) => offset = position + 4,
        }
    }
    if chunks.is_empty() {
        return Err("No compressed thumbnail data found".into());
    }

    if category == "bodies" {
        let marker = [0u8, 0, 4, 0, 0, 0, 4, 0]; // two little-endian 262144 values
        for chunk in chunks.iter().skip(usize::from(chunks.len() > 1)) {
            if let Some(position) = chunk
                .windows(marker.len())
                .position(|window| window == marker)
            {
                if let Ok(png) = bgra_png(&chunk[position + marker.len()..], 256, 256) {
                    return Ok(png);
                }
            }
        }
    }
    let joined: Vec<u8> = chunks.iter().flatten().copied().collect();
    if let Some((pixels, width, height)) = generic_frame(&joined, category) {
        return bgra_png(pixels, width, height);
    }
    // Wheels use the adaptive tail layout when present. The fixed 645-byte
    // layout is only the legacy fallback; trying it first corrupts newer wheel
    // thumbnails that happen to be large enough.
    if category == "wheels" && chunks[0].len() >= 645 + 256 * 256 * 4 {
        return bgra_png(&chunks[0][645..], 256, 256);
    }
    Err("No supported BGRA thumbnail frame found".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn item_thumbnail_uses_last_square_frame_from_rectangular_match() {
        let mut bytes = vec![0; 100];
        bytes.extend(vec![1; 256 * 256 * 4]);
        bytes.extend(vec![2; 256 * 256 * 4]);
        bytes.extend(vec![0; TRAILER_SIZE]);

        let (frame, width, height) = generic_frame(&bytes, "engines").unwrap();
        assert_eq!((width, height), (256, 256));
        assert_eq!(frame.len(), 256 * 256 * 4);
        assert!(frame.iter().all(|byte| *byte == 2));
    }

    #[test]
    fn player_banner_keeps_rectangular_frame() {
        let mut bytes = vec![0; 100];
        bytes.extend(vec![1; 512 * 256 * 4]);
        bytes.extend(vec![0; TRAILER_SIZE]);

        let (frame, width, height) = generic_frame(&bytes, "banners").unwrap();
        assert_eq!((width, height), (512, 256));
        assert_eq!(frame.len(), 512 * 256 * 4);
    }

    #[test]
    #[ignore = "requires a local Rocket League installation"]
    fn extracts_known_thumbnail_formats() {
        let cooked = Path::new(r"C:\Program Files\Epic Games\rocketleague\TAGame\CookedPCConsole");
        let output = Path::new(env!("CARGO_MANIFEST_DIR")).join(".codex-target/thumb-tests");
        std::fs::create_dir_all(&output).unwrap();
        for (file, category) in [
            ("body_Amber_T_SF.upk", "bodies"),
            ("Body_Aftershock_T_SF.upk", "body-aftershock"),
            ("EngineAudio_Aftershock_OE_T_SF.upk", "engine-aftershock"),
            ("EngineAudio_Aftershock_V2_T_SF.upk", "engine-aftershock-v2"),
            ("EngineAudio_Alokin_T_SF.upk", "engine-alokin"),
            ("WHEEL_Vortex_T_SF.upk", "wheels"),
            ("Boost_Standard_T_SF.upk", "boosts"),
        ] {
            let decode_category = if category.starts_with("body-") {
                "bodies"
            } else if category.starts_with("engine-") {
                "engines"
            } else {
                category
            };
            let png = extract_png(&cooked.join(file), decode_category).unwrap();
            let decoded = image::load_from_memory(&png).unwrap();
            if category.starts_with("engine-") {
                assert_eq!((decoded.width(), decoded.height()), (256, 256));
            }
            std::fs::write(output.join(format!("{category}.png")), png).unwrap();
        }
    }
}
