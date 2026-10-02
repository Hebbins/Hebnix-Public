use crate::patch_core::upk;
use crate::patcher::upk_package::{ExportEntry, UpkPackage};
use image::{DynamicImage, ImageFormat, RgbaImage};
use std::io::{Cursor, Read, Seek, SeekFrom};
use std::path::Path;

const MAX_TEXTURE_BYTES: usize = 64 * 1024 * 1024;

fn word(bytes: &[u8], at: &mut usize) -> Result<u32, String> {
    let data = bytes.get(*at..*at + 4).ok_or("Truncated texture record")?;
    *at += 4;
    Ok(u32::from_le_bytes(data.try_into().unwrap()))
}

struct Bulk<'a> {
    flags: u32,
    size: usize,
    disk_size: usize,
    offset: u64,
    inline: &'a [u8],
}

fn bulk<'a>(bytes: &'a [u8], at: &mut usize, offset_width: usize) -> Result<Bulk<'a>, String> {
    let flags = word(bytes, at)?;
    let size = word(bytes, at)? as usize;
    let stored_size = word(bytes, at)?;
    // Stripped cooked mips use -1 for their stored size and have no pixels.
    let disk_size = if flags & 32 != 0 && size == 0 && stored_size == u32::MAX {
        0
    } else {
        stored_size as usize
    };
    if size > MAX_TEXTURE_BYTES || disk_size > MAX_TEXTURE_BYTES {
        return Err(format!(
            "Texture bulk data exceeds size limit: flags={flags:x} size={size} disk={disk_size} at={at}"
        ));
    }
    // Rocket League omits the offset when BULKDATA_NoOffsetFixUp is set.
    let offset = if flags & 0x10000 != 0 && flags & 1 == 0 {
        0
    } else {
        let low = word(bytes, at)? as u64;
        if offset_width == 8 {
            low | ((word(bytes, at)? as u64) << 32)
        } else {
            low
        }
    };
    let inline = if flags & (1 | 32) == 0 {
        let data = bytes
            .get(*at..*at + disk_size)
            .ok_or("Truncated inline mip")?;
        *at += disk_size;
        data
    } else {
        &[]
    };
    Ok(Bulk {
        flags,
        size,
        disk_size,
        offset,
        inline,
    })
}

fn bulk_bytes(record: &Bulk<'_>, directory: &Path, cache: Option<&str>) -> Result<Vec<u8>, String> {
    if record.flags & 32 != 0 || record.size == 0 {
        return Err("Mip has no stored pixels".into());
    }
    let stored = if record.flags & 1 != 0 {
        let cache = cache.ok_or("External mip has no texture cache name")?;
        if cache.contains(['/', '\\', ':']) || cache == ".." {
            return Err("Invalid texture cache name".into());
        }
        let mut file = std::fs::File::open(directory.join(format!("{cache}.tfc")))
            .map_err(|e| format!("{cache}.tfc: {e}"))?;
        file.seek(SeekFrom::Start(record.offset))
            .map_err(|e| e.to_string())?;
        let mut bytes = vec![0; record.disk_size];
        file.read_exact(&mut bytes).map_err(|e| e.to_string())?;
        bytes
    } else {
        record.inline.to_vec()
    };
    let bytes = if record.flags & 2 != 0 {
        upk::decomp_chunk_at(&stored, 0)
            .map_err(|e| format!("Mip decompression: {e:?}"))?
            .0
    } else if record.flags & (16 | 128) != 0 {
        return Err("Unsupported mip compression".into());
    } else {
        stored
    };
    if bytes.len() != record.size {
        return Err("Mip byte count mismatch".into());
    }
    Ok(bytes)
}

fn decode_pixels(
    bytes: &[u8],
    width: usize,
    height: usize,
    format: &str,
) -> Result<RgbaImage, String> {
    if width == 0 || height == 0 || width > 4096 || height > 4096 {
        return Err("Invalid thumbnail dimensions".into());
    }
    let block_size = match format {
        "PF_DXT1" => 8,
        "PF_DXT3" | "PF_DXT5" | "PF_BC7" => 16,
        "PF_A8R8G8B8" => 0,
        _ => return Err(format!("Unsupported thumbnail pixel format: {format}")),
    };
    let expected = if block_size == 0 {
        width * height * 4
    } else {
        width.div_ceil(4) * height.div_ceil(4) * block_size
    };
    if bytes.len() != expected {
        return Err(format!(
            "Invalid {format} mip size: {} != {expected}",
            bytes.len()
        ));
    }
    let mut rgba = vec![0; width * height * 4];
    if block_size == 0 {
        rgba.copy_from_slice(bytes);
        for pixel in rgba.chunks_exact_mut(4) {
            pixel.swap(0, 2);
        }
    } else {
        for by in 0..height.div_ceil(4) {
            for bx in 0..width.div_ceil(4) {
                let offset = (by * width.div_ceil(4) + bx) * block_size;
                let mut block = [0; 64];
                match format {
                    "PF_DXT1" => bcdec_rs::bc1(&bytes[offset..offset + block_size], &mut block, 16),
                    "PF_DXT3" => bcdec_rs::bc2(&bytes[offset..offset + block_size], &mut block, 16),
                    "PF_BC7" => bcdec_rs::bc7(&bytes[offset..offset + block_size], &mut block, 16),
                    _ => bcdec_rs::bc3(&bytes[offset..offset + block_size], &mut block, 16),
                }
                for y in 0..4.min(height - by * 4) {
                    for x in 0..4.min(width - bx * 4) {
                        let dst = ((by * 4 + y) * width + bx * 4 + x) * 4;
                        let src = (y * 4 + x) * 4;
                        rgba[dst..dst + 4].copy_from_slice(&block[src..src + 4]);
                    }
                }
            }
        }
    }
    RgbaImage::from_raw(width as u32, height as u32, rgba).ok_or("Invalid RGBA image".into())
}

pub(crate) fn texture(
    package: &UpkPackage,
    export: &ExportEntry,
    directory: &Path,
) -> Result<RgbaImage, String> {
    let (props, mut at) = package.serialized_props(export)?;
    let serial = package
        .image
        .get(export.serial_offset..export.serial_offset + export.serial_size)
        .ok_or("Truncated texture")?;
    let name_value = |name: &str| -> Option<&str> {
        let prop = props.iter().find(|p| p.name == name)?;
        let mut pos = prop.value_offset;
        package
            .names
            .get(word(serial, &mut pos).ok()? as usize)
            .map(String::as_str)
    };
    let format = name_value("Format").ok_or("Texture format is missing")?;
    let cache = name_value("TextureFileCacheName");
    // Native Texture2D serialization: source-art bulk, mip count, then each
    // mip's bulk record followed by its actual width and height.
    bulk(serial, &mut at, package.bulk_offset_width())?;
    let count = word(serial, &mut at)?;
    if count == 0 || count > 32 {
        return Err("Invalid thumbnail mip count".into());
    }
    let mut last_error = "No readable thumbnail mip".to_string();
    for _ in 0..count {
        let record = bulk(serial, &mut at, package.bulk_offset_width())?;
        let width = word(serial, &mut at)? as usize;
        let height = word(serial, &mut at)? as usize;
        match bulk_bytes(&record, directory, cache)
            .and_then(|bytes| decode_pixels(&bytes, width, height, format))
        {
            Ok(image) => return Ok(image),
            Err(error) => last_error = error,
        }
    }
    Err(last_error)
}

/// Strict validation for the self-contained car replacement textures.
pub(crate) fn validate_resident_texture(
    package: &UpkPackage,
    export: &ExportEntry,
) -> Result<usize, String> {
    let (props, mut at) = package.serialized_props(export)?;
    let serial = &package.image[export.serial_offset..export.serial_offset + export.serial_size];
    if !props
        .iter()
        .any(|p| p.name == "NeverStream" && p.bool_value == Some(true))
    {
        return Err("Texture is streamable".into());
    }
    let named = |name: &str| -> Result<&str, String> {
        let prop = props
            .iter()
            .find(|p| p.name == name)
            .ok_or(format!("Missing {name}"))?;
        let mut pos = prop.value_offset;
        package
            .names
            .get(word(serial, &mut pos)? as usize)
            .map(String::as_str)
            .ok_or("Invalid texture name".into())
    };
    if named("TextureFileCacheName")? != "None" {
        return Err("Texture retains external cache".into());
    }
    let format = named("Format")?;
    bulk(serial, &mut at, package.bulk_offset_width())?;
    let count = word(serial, &mut at)?;
    if count == 0 || count > 16 {
        return Err("Invalid resident mip count".into());
    }
    let mut previous: Option<(usize, usize)> = None;
    for _ in 0..count {
        let record = bulk(serial, &mut at, package.bulk_offset_width())?;
        if record.flags & (1 | 32 | 64) != 0 {
            return Err("Mip is not inline".into());
        }
        let width = word(serial, &mut at)? as usize;
        let height = word(serial, &mut at)? as usize;
        if let Some((w, h)) = previous {
            if (width, height) != ((w / 2).max(1), (h / 2).max(1)) {
                return Err("Broken mip dimensions".into());
            }
        }
        decode_pixels(record.inline, width, height, format)?;
        previous = Some((width, height));
    }
    if previous != Some((1, 1)) {
        return Err("Incomplete mip chain".into());
    }
    Ok(count as usize)
}

pub fn extract_png(path: &Path, _category: &str) -> Result<Vec<u8>, String> {
    let package = UpkPackage::load(path)?;
    let directory = path.parent().ok_or("Thumbnail has no parent directory")?;
    // Follow ProductThumbnailAsset's reference, rather than choosing whichever
    // Texture2D happens to occur last in a multi-texture package.
    let mut indices = Vec::new();
    for export in &package.exports {
        if !package
            .class_of(export)
            .starts_with("ProductThumbnailAsset")
        {
            continue;
        }
        if let Ok((props, _)) = package.serialized_props(export) {
            for prop in props
                .iter()
                .filter(|p| p.name == "Thumbnail" && p.tag_type == "ObjectProperty")
            {
                if let Ok(reference) = package.read_int(export.serial_offset + prop.value_offset) {
                    if reference > 0 {
                        indices.push((reference - 1) as usize);
                    }
                }
            }
        }
    }
    if indices.is_empty() {
        indices.extend(
            package
                .exports
                .iter()
                .enumerate()
                .filter(|(_, e)| package.class_of(e).starts_with("Texture2D"))
                .map(|(i, _)| i),
        );
    }
    let mut errors = Vec::new();
    for index in indices {
        let Some(export) = package.exports.get(index) else {
            continue;
        };
        if !package.class_of(export).starts_with("Texture2D") {
            continue;
        }
        match texture(&package, export, directory) {
            Ok(image) => {
                let image = DynamicImage::ImageRgba8(image);
                let image = if image.width() > 512 || image.height() > 512 {
                    image.thumbnail(512, 512)
                } else {
                    image
                };
                let mut output = Cursor::new(Vec::new());
                image
                    .write_to(&mut output, ImageFormat::Png)
                    .map_err(|e| e.to_string())?;
                return Ok(output.into_inner());
            }
            Err(error) => errors.push(error),
        }
    }
    Err(format!(
        "No readable thumbnail in {}: {}",
        path.display(),
        errors.join("; ")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dxt5_preserves_alpha_including_small_mips() {
        for size in [1, 2, 4, 8] {
            for alpha in [0, 63, 128, 255] {
                let pixels = RgbaImage::from_pixel(size, size, image::Rgba([128, 64, 32, alpha]));
                let bytes = encode_dxt5_alpha(&pixels, size as usize, size as usize);
                let decoded =
                    decode_pixels(&bytes, size as usize, size as usize, "PF_DXT5").unwrap();
                assert!(decoded.pixels().all(|p| p[3] == alpha));
            }
        }
        let pixels = RgbaImage::from_fn(4, 4, |x, y| {
            image::Rgba([255, 255, 255, ((y * 4 + x) * 17) as u8])
        });
        let bytes = encode_dxt5_alpha(&pixels, 4, 4);
        let decoded = decode_pixels(&bytes, 4, 4, "PF_DXT5").unwrap();
        for (a, b) in pixels.pixels().zip(decoded.pixels()) {
            assert!((a[3] as i16 - b[3] as i16).abs() <= 19);
        }
    }

    #[test]
    #[ignore = "Audits installed boost/banner packages; set HEBNIX_THUMBNAIL_DIR"]
    fn audit_local_thumbnails() {
        let directory = std::env::var_os("HEBNIX_THUMBNAIL_DIR").expect("Set HEBNIX_THUMBNAIL_DIR");
        let mut success = 0;
        let mut failures = 0;
        for entry in std::fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            let name = path
                .file_name()
                .unwrap()
                .to_string_lossy()
                .to_ascii_lowercase();
            if !name.ends_with("_t_sf.upk")
                || !(name.starts_with("boost_") || name.starts_with("playerbanner_"))
            {
                continue;
            }
            match extract_png(&path, "") {
                Ok(_) => success += 1,
                Err(error) => {
                    failures += 1;
                    println!("FAILED {name}: {error}");
                }
            }
        }
        println!("AUDIT: {success} decoded, {failures} unavailable");
        assert!(success > 0);
        assert!(success * 100 >= (success + failures) * 95, "Thumbnail audit fell below 95%: {success} decoded, {failures} unavailable");
    }

    #[test]
    fn decodes_bgra_and_rejects_wrong_sizes() {
        let image = decode_pixels(&[1, 2, 3, 255], 1, 1, "PF_A8R8G8B8").unwrap();
        assert_eq!(image.as_raw(), &[3, 2, 1, 255]);
        assert!(decode_pixels(&[0; 8], 4, 4, "PF_A8R8G8B8").is_err());
    }

    #[test]
    fn decodes_dxt1_with_partial_edge_blocks() {
        let image = decode_pixels(&[0, 248, 0, 0, 0, 0, 0, 0], 3, 2, "PF_DXT1").unwrap();
        assert!(image.pixels().all(|p| p.0 == [255, 0, 0, 255]));
    }

    #[test]
    fn bulk_offsets_and_inline_boundaries() {
        let mut data = Vec::new();
        for value in [0x10003u32, 32768, 100, 12345, 1, 256, 256] {
            data.extend_from_slice(&value.to_le_bytes());
        }
        let mut at = 0;
        let record = bulk(&data, &mut at, 8).unwrap();
        assert_eq!(record.offset, (1u64 << 32) + 12345);
        assert_eq!(at, 20);
        assert!(record.inline.is_empty());
        assert_eq!(word(&data, &mut at).unwrap(), 256);

        let mut data = Vec::new();
        for value in [0x10000u32, 4, 4, 0x04030201, 1, 1] {
            data.extend_from_slice(&value.to_le_bytes());
        }
        let mut at = 0;
        let record = bulk(&data, &mut at, 8).unwrap();
        assert_eq!(record.inline, &[1, 2, 3, 4]);
        assert_eq!(at, 16);
        assert!(bulk(&data[..14], &mut 0, 8).is_err());
    }

    #[test]
    #[ignore = "Requires a local Rocket League installation; set HEBNIX_THUMBNAIL_DIR"]
    fn local_thumbnail_regression() {
        let directory = std::env::var_os("HEBNIX_THUMBNAIL_DIR").expect("Set HEBNIX_THUMBNAIL_DIR");
        let mut failures = Vec::new();
        for name in [
            "Body_Grain",
            "Hat_Halo",
            "Skin_Zomba",
            "Antenna_8Ball",
            "EngineAudio_AA",
            "AvatarBorder_10Year",
            "Boost_AlphaReward",
            "boost_lp_fire",
            "boost_sphenergy",
            "boost_2d_smoke",
            "boost_aurawater",
            "PlayerBanner_MCD",
            "PlayerBanner_Anniv23",
            "PlayerBanner_barrel",
            "PlayerBanner_RetroAbstract",
        ] {
            let path = std::path::PathBuf::from(&directory).join(format!("{name}_T_SF.upk"));
            let png = match extract_png(&path, "boosts") {
                Ok(png) => png,
                Err(error) => {
                    failures.push(format!("{name}: {error}"));
                    continue;
                }
            };
            let image = image::load_from_memory(&png).unwrap();
            println!("{name}: {}x{}", image.width(), image.height());
            if let Some(output) = std::env::var_os("HEBNIX_THUMBNAIL_OUTPUT") {
                std::fs::create_dir_all(&output).unwrap();
                std::fs::write(
                    std::path::PathBuf::from(output).join(format!("{name}.png")),
                    png,
                )
                .unwrap();
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }
}

/// Bake an opaque image into the existing Texture2D, with a complete inline mip
/// chain. No shared TFC is edited and no absolute bulk-data pointers are emitted.
pub(crate) fn bake_texture(
    package: &UpkPackage,
    export: &ExportEntry,
    pixels: &RgbaImage,
) -> Result<Vec<u8>, String> {
    bake_texture_encoded(package, export, pixels, false)
}

/// Preserve mask alpha using an inline DXT5 mip chain.
pub(crate) fn bake_texture_alpha(
    package: &UpkPackage,
    export: &ExportEntry,
    pixels: &RgbaImage,
) -> Result<Vec<u8>, String> {
    let mut bytes = bake_texture_encoded(package, export, pixels, true)?;
    let (props, native) = package.serialized_props(export)?;
    let name = |s: &str| {
        package
            .names
            .iter()
            .position(|n| n == s)
            .map(|i| i as u32)
            .ok_or_else(|| format!("Missing texture property name {s}"))
    };
    // These mips are embedded. A retained TFC name permits the streaming path
    // to read the old cache using offsets belonging to the rebuilt package.
    if let Some(cache) = props.iter().find(|p| p.name == "TextureFileCacheName") {
        bytes[cache.value_offset..cache.value_offset + 4]
            .copy_from_slice(&name("None")?.to_le_bytes());
        bytes[cache.value_offset + 4..cache.value_offset + 8].fill(0);
    }
    if let Some(stream) = props.iter().find(|p| p.name == "NeverStream") {
        bytes[stream.value_offset - 1] = 1;
    } else {
        let mut tag = Vec::new();
        for n in [name("NeverStream")?, 0, name("BoolProperty")?, 0, 0, 0] {
            tag.extend_from_slice(&n.to_le_bytes());
        }
        tag.push(1);
        bytes.splice(native - 8..native - 8, tag);
    }
    Ok(bytes)
}

fn encode_dxt5_alpha(pixels: &RgbaImage, width: usize, height: usize) -> Vec<u8> {
    let mut bytes = crate::patcher::patch_core::dxt::image_to_dxt5(pixels, width, height);
    for by in 0..height.div_ceil(4) {
        for bx in 0..width.div_ceil(4) {
            let mut values = [0u8; 16];
            for y in 0..4 {
                for x in 0..4 {
                    values[y * 4 + x] = pixels.get_pixel(
                        (bx * 4 + x).min(width - 1) as u32,
                        (by * 4 + y).min(height - 1) as u32,
                    )[3];
                }
            }
            let hi = *values.iter().max().unwrap();
            let lo = *values.iter().min().unwrap();
            let mut palette = [0u8; 8];
            palette[0] = hi;
            palette[1] = lo;
            if hi > lo {
                for i in 1..7 {
                    palette[i + 1] =
                        (((7 - i) as u16 * hi as u16 + i as u16 * lo as u16) / 7) as u8;
                }
            } else {
                palette.fill(hi);
            }
            let mut indices = 0u64;
            for (i, value) in values.iter().enumerate() {
                let selected = (0..8)
                    .min_by_key(|&j| (palette[j] as i16 - *value as i16).abs())
                    .unwrap();
                indices |= (selected as u64) << (3 * i);
            }
            let at = (by * width.div_ceil(4) + bx) * 16;
            bytes[at] = hi;
            bytes[at + 1] = lo;
            bytes[at + 2..at + 8].copy_from_slice(&indices.to_le_bytes()[..6]);
        }
    }
    bytes
}

fn bake_texture_encoded(
    package: &UpkPackage,
    export: &ExportEntry,
    pixels: &RgbaImage,
    alpha: bool,
) -> Result<Vec<u8>, String> {
    let (props, native) = package.serialized_props(export)?;
    let serial = &package.image[export.serial_offset..export.serial_offset + export.serial_size];
    let mut at = native;
    let art = bulk(serial, &mut at, package.bulk_offset_width())?;
    if art.size != 0 {
        return Err("Unexpected source art in cooked ball texture".into());
    }
    let count = word(serial, &mut at)?;
    if count == 0 || count > 32 {
        return Err("Invalid ball mip count".into());
    }
    for _ in 0..count {
        bulk(serial, &mut at, package.bulk_offset_width())?;
        word(serial, &mut at)?;
        word(serial, &mut at)?;
    }
    // Keep the inspected native tail (GUID and empty cached-platform data).
    let tail = &serial[at..];
    let empty_tail: Vec<u8> = [0i32, 0, 0, 33, 0, -1, -1, -1, 0]
        .into_iter()
        .flat_map(i32::to_le_bytes)
        .collect();
    if tail.len() != 52 || tail[16..] != empty_tail {
        return Err(format!(
            "Unsupported cached texture tail ({} bytes): {tail:?}",
            tail.len()
        ));
    }
    let mut output = serial[..native].to_vec();
    let size = pixels
        .width()
        .max(pixels.height())
        .min(1024)
        .next_power_of_two();
    let mips = size.ilog2() + 1;
    let format = package
        .names
        .iter()
        .position(|n| n == if alpha { "PF_DXT5" } else { "PF_DXT1" })
        .ok_or("Required DXT format name missing")?;
    for prop in &props {
        let value = match prop.name.as_str() {
            "SizeX" | "SizeY" | "OriginalSizeX" | "OriginalSizeY" => Some(size),
            "MipTailBaseIdx" => Some(mips - 1),
            "FirstResourceMemMip" => Some(0),
            "Format" => Some(format as u32),
            _ => None,
        };
        if let Some(value) = value {
            output[prop.value_offset..prop.value_offset + 4].copy_from_slice(&value.to_le_bytes());
        }
    }
    let push = |out: &mut Vec<u8>, n: u32| out.extend_from_slice(&n.to_le_bytes());
    for n in [0x10000, 0, 0, mips] {
        push(&mut output, n);
    }
    for level in 0..mips {
        let width = (size >> level).max(1);
        let resized =
            image::imageops::resize(pixels, width, width, image::imageops::FilterType::Triangle);
        let encode = if alpha {
            encode_dxt5_alpha
        } else {
            crate::patcher::patch_core::dxt::image_to_dxt1
        };
        let bytes = encode(&resized, width as usize, width as usize);
        for n in [0x10000, bytes.len() as u32, bytes.len() as u32] {
            push(&mut output, n);
        }
        output.extend_from_slice(&bytes);
        push(&mut output, width);
        push(&mut output, width);
    }
    output.extend_from_slice(tail);
    Ok(output)
}
