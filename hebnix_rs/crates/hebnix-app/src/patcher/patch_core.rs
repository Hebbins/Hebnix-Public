#![allow(dead_code)]
#![allow(unused_imports)]
#![allow(unused_variables)]
#![allow(unused_mut)]

pub mod dxt {
    use image::{Pixel, RgbaImage};

    const ALPHA_BLOCK_255: [u8; 8] = [255, 0, 0, 0, 0, 0, 0, 0];

    /// Match BallPatcher.LoadPngCompositeResize: the ball diffuse is DXT1,
    /// so source alpha must be flattened before it is discarded.
    pub fn composite_over_white(mut img: RgbaImage) -> RgbaImage {
        for pixel in img.pixels_mut() {
            let alpha = pixel[3] as u32;
            for channel in 0..3 {
                pixel[channel] =
                    ((pixel[channel] as u32 * alpha + 255 * (255 - alpha) + 127) / 255) as u8;
            }
            pixel[3] = 255;
        }
        img
    }

    #[inline]
    pub fn rgb_to_rgb565(r: i32, g: i32, b: i32) -> u16 {
        ((r >> 3) << 11 | (g >> 2) << 5 | (b >> 3)) as u16
    }

    #[inline]
    pub fn rgb565_to_rgb(v: u16) -> (i32, i32, i32) {
        let v = v as i32;
        ((v >> 11) << 3, ((v >> 5) & 0x3F) << 2, (v & 0x1F) << 3)
    }

    pub fn dxt1_block(r: &[i32; 16], g: &[i32; 16], b: &[i32; 16]) -> [u8; 8] {
        let mut max_rgb565: i32 = -1;
        let mut min_rgb565: i32 = -1;

        for i in 0..16 {
            let c_rgb565 = rgb_to_rgb565(r[i], g[i], b[i]) as i32;
            if max_rgb565 < 0 || c_rgb565 > max_rgb565 {
                max_rgb565 = c_rgb565;
            }
            if min_rgb565 < 0 || c_rgb565 < min_rgb565 {
                min_rgb565 = c_rgb565;
            }
        }

        if max_rgb565 < min_rgb565 {
            std::mem::swap(&mut max_rgb565, &mut min_rgb565);
        }

        if max_rgb565 == min_rgb565 {
            if max_rgb565 < 65535 {
                max_rgb565 += 1;
            } else {
                min_rgb565 -= 1;
            }
        }

        let rgb565_0 = max_rgb565 as u16;
        let rgb565_1 = min_rgb565 as u16;
        let mut color_indices = 0u32;

        let (r0, g0, b0) = rgb565_to_rgb(rgb565_0);
        let (r1, g1, b1) = rgb565_to_rgb(rgb565_1);

        let color_palette = [
            [r0, g0, b0],
            [r1, g1, b1],
            [(2 * r0 + r1) / 3, (2 * g0 + g1) / 3, (2 * b0 + b1) / 3],
            [(r0 + 2 * r1) / 3, (g0 + 2 * g1) / 3, (b0 + 2 * b1) / 3],
        ];

        for i in 0..16 {
            let mut num2 = 0;
            let mut num3 = i32::MAX;
            for j in 0..4 {
                let num4 = r[i] - color_palette[j][0];
                let num5 = g[i] - color_palette[j][1];
                let num6 = b[i] - color_palette[j][2];
                let num7 = num4 * num4 + num5 * num5 + num6 * num6;
                if num7 < num3 {
                    num3 = num7;
                    num2 = j;
                }
            }
            color_indices |= (num2 as u32) << (i * 2);
        }

        let mut out = [0u8; 8];
        out[0..2].copy_from_slice(&rgb565_0.to_le_bytes());
        out[2..4].copy_from_slice(&rgb565_1.to_le_bytes());
        out[4..8].copy_from_slice(&color_indices.to_le_bytes());
        out
    }

    pub fn image_to_dxt1(img: &RgbaImage, width: usize, height: usize) -> Vec<u8> {
        let mut dxt1 = vec![0u8; width.max(4) / 4 * height.max(4) / 4 * 8];
        let mut out_idx = 0;

        let mut r = [0i32; 16];
        let mut g = [0i32; 16];
        let mut b = [0i32; 16];

        for index1 in (0..height).step_by(4) {
            for index2 in (0..width).step_by(4) {
                for index3 in 0..4 {
                    for index4 in 0..4 {
                        let px_x = (index2 + index4).min(width - 1) as u32;
                        let px_y = (index1 + index3).min(height - 1) as u32;

                        let pixel = img.get_pixel(px_x, px_y).channels();
                        let index6 = index3 * 4 + index4;

                        r[index6] = pixel[0] as i32;
                        g[index6] = pixel[1] as i32;
                        b[index6] = pixel[2] as i32;
                    }
                }

                let block = dxt1_block(&r, &g, &b);
                dxt1[out_idx..out_idx + 8].copy_from_slice(&block);
                out_idx += 8;
            }
        }
        dxt1
    }

    pub fn image_to_dxt5(img: &RgbaImage, width: usize, height: usize) -> Vec<u8> {
        let mut dxt5 = vec![0u8; width.max(4) / 4 * height.max(4) / 4 * 16];
        let mut out_idx = 0;

        let mut r = [0i32; 16];
        let mut g = [0i32; 16];
        let mut b = [0i32; 16];

        for index1 in (0..height).step_by(4) {
            for index2 in (0..width).step_by(4) {
                for index3 in 0..4 {
                    for index4 in 0..4 {
                        let px_x = (index2 + index4).min(width - 1) as u32;
                        let px_y = (index1 + index3).min(height - 1) as u32;

                        let pixel = img.get_pixel(px_x, px_y).channels();
                        let index6 = index3 * 4 + index4;

                        r[index6] = pixel[0] as i32;
                        g[index6] = pixel[1] as i32;
                        b[index6] = pixel[2] as i32;
                    }
                }

                dxt5[out_idx..out_idx + 8].copy_from_slice(&ALPHA_BLOCK_255);
                let block = dxt1_block(&r, &g, &b);
                dxt5[out_idx + 8..out_idx + 16].copy_from_slice(&block);

                out_idx += 16;
            }
        }
        dxt5
    }

    pub fn decode_dxt1(dxt_bytes: &[u8], width: usize, height: usize) -> RgbaImage {
        let mut img = RgbaImage::new(width as u32, height as u32);
        let mut block_idx = 0;

        for index1 in (0..height).step_by(4) {
            for index2 in (0..width).step_by(4) {
                let offset = block_idx * 8;
                block_idx += 1;

                let uint16_1 =
                    u16::from_le_bytes(dxt_bytes[offset..offset + 2].try_into().unwrap());
                let uint16_2 =
                    u16::from_le_bytes(dxt_bytes[offset + 2..offset + 4].try_into().unwrap());

                let (r1, g1, b1) = rgb565_to_rgb(uint16_1);
                let (r2, g2, b2) = rgb565_to_rgb(uint16_2);
                let uint32 =
                    u32::from_le_bytes(dxt_bytes[offset + 4..offset + 8].try_into().unwrap());

                let num_array = if uint16_1 > uint16_2 {
                    [
                        [r1, g1, b1],
                        [r2, g2, b2],
                        [(2 * r1 + r2) / 3, (2 * g1 + g2) / 3, (2 * b1 + b2) / 3],
                        [(r1 + 2 * r2) / 3, (g1 + 2 * g2) / 3, (b1 + 2 * b2) / 3],
                    ]
                } else {
                    [
                        [r1, g1, b1],
                        [r2, g2, b2],
                        [(r1 + r2) / 2, (g1 + g2) / 2, (b1 + b2) / 2],
                        [0, 0, 0],
                    ]
                };

                for index3 in 0..4 {
                    for index4 in 0..4 {
                        let px_x = index2 + index4;
                        let px_y = index1 + index3;

                        if px_x < width && px_y < height {
                            let color_idx = ((uint32 >> ((index3 * 4 + index4) * 2)) & 3) as usize;
                            let rgb = num_array[color_idx];

                            img.put_pixel(
                                px_x as u32,
                                px_y as u32,
                                image::Rgba([rgb[0] as u8, rgb[1] as u8, rgb[2] as u8, 255]),
                            );
                        }
                    }
                }
            }
        }
        img
    }
}

pub mod upk {
    use flate2::Compression;
    use flate2::read::ZlibDecoder;
    use flate2::write::ZlibEncoder;
    use std::io::{Read, Write};

    pub const UPK_MAGIC: u32 = 2653586369; // 0x9E2A83C1
    const VALID_BLK_SZ: [u32; 6] = [32768, 65536, 131072, 262144, 524288, 1048576];

    #[derive(Debug)]
    pub enum UpkError {
        Io(std::io::Error),
        InvalidMagic,
        InvalidBlockSize,
        DecompressionFailed,
        OutOfBounds,
        OversizedChunk,
    }

    impl From<std::io::Error> for UpkError {
        fn from(err: std::io::Error) -> Self {
            UpkError::Io(err)
        }
    }

    pub fn zlib_compress(data: &[u8], level: u32) -> Result<Vec<u8>, UpkError> {
        let comp_level = match level {
            0 => Compression::none(),
            1..=3 => Compression::fast(),
            9 => Compression::best(),
            _ => Compression::default(),
        };
        let mut encoder = ZlibEncoder::new(Vec::new(), comp_level);
        encoder.write_all(data)?;
        Ok(encoder.finish()?)
    }

    pub fn zlib_decompress(data: &[u8]) -> Result<Vec<u8>, UpkError> {
        let mut decoder = ZlibDecoder::new(data);
        let mut out = Vec::new();
        decoder.read_to_end(&mut out)?;
        Ok(out)
    }

    pub fn compress_sub(data: &[u8], target: usize) -> Result<Vec<u8>, UpkError> {
        let mut best_comp = zlib_compress(data, 9)?;
        if best_comp.len() <= target {
            return Ok(best_comp);
        }

        let levels = [6, 3, 1];
        for level in levels {
            let comp = zlib_compress(data, level)?;
            if comp.len() < best_comp.len() {
                best_comp = comp;
            }
            if best_comp.len() <= target {
                break;
            }
        }
        Ok(best_comp)
    }

    #[inline]
    pub fn read_u32(buf: &[u8], off: usize) -> u32 {
        u32::from_le_bytes(buf[off..off + 4].try_into().unwrap())
    }

    #[inline]
    pub fn write_u32(buf: &mut [u8], off: usize, val: u32) {
        buf[off..off + 4].copy_from_slice(&val.to_le_bytes());
    }

    pub fn decomp_chunk_at(
        file_bytes: &[u8],
        chunk_pos: usize,
    ) -> Result<(Vec<u8>, u32, usize), UpkError> {
        if chunk_pos + 16 > file_bytes.len() {
            return Err(UpkError::OutOfBounds);
        }

        let magic = read_u32(file_bytes, chunk_pos);
        if magic != UPK_MAGIC {
            return Err(UpkError::InvalidMagic);
        }

        let blk_sz = read_u32(file_bytes, chunk_pos + 4);
        let unc_tot = read_u32(file_bytes, chunk_pos + 12);

        if !VALID_BLK_SZ.contains(&blk_sz) || unc_tot <= 1 || unc_tot >= 50_000_000 {
            return Err(UpkError::InvalidBlockSize);
        }

        let length = ((unc_tot + blk_sz - 1) / blk_sz) as usize;
        let header_end = chunk_pos + 16 + length * 8;

        if header_end > file_bytes.len() {
            return Err(UpkError::OutOfBounds);
        }

        let mut block_sizes = Vec::with_capacity(length);
        for i in 0..length {
            block_sizes.push(read_u32(file_bytes, chunk_pos + 16 + i * 8) as usize);
        }

        let mut decompressed = Vec::with_capacity(unc_tot as usize);
        let mut offset = header_end;

        for size in block_sizes {
            if offset + size > file_bytes.len() {
                return Err(UpkError::OutOfBounds);
            }
            let dec = zlib_decompress(&file_bytes[offset..offset + size])
                .map_err(|_| UpkError::DecompressionFailed)?;
            decompressed.extend_from_slice(&dec);
            offset += size;
        }

        Ok((decompressed, blk_sz, offset))
    }

    pub fn recomp_chunk_safely_padded(
        dec_bytes: &[u8],
        blk_sz: usize,
        target_file_size: Option<usize>,
    ) -> Result<Vec<u8>, UpkError> {
        let length = (dec_bytes.len() + blk_sz - 1) / blk_sz;
        let mut compressed_blocks = Vec::with_capacity(length);
        let mut uncompressed_sizes = Vec::with_capacity(length);

        for i in 0..length {
            let num1 = i * blk_sz;
            let num2 = blk_sz.min(dec_bytes.len() - num1);
            let slice = &dec_bytes[num1..num1 + num2];

            let comp = zlib_compress(slice, 9)?;
            uncompressed_sizes.push(slice.len() as u32);
            compressed_blocks.push(comp);
        }

        let mut total_comp_size: usize = compressed_blocks.iter().map(|b| b.len()).sum();
        let header_len = 16 + length * 8;

        let mut padding_required = 0;
        if let Some(target) = target_file_size {
            let current_size = header_len + total_comp_size;
            if current_size < target {
                padding_required = target - current_size;
                total_comp_size += padding_required;
            } else if current_size > target {
                return Err(UpkError::OversizedChunk);
            }
        }

        let mut out = vec![0u8; header_len + total_comp_size];

        write_u32(&mut out, 0, UPK_MAGIC);
        write_u32(&mut out, 4, blk_sz as u32);
        write_u32(&mut out, 8, total_comp_size as u32);
        write_u32(&mut out, 12, dec_bytes.len() as u32);

        let mut offset = header_len;
        for i in 0..length {
            let mut comp_len = compressed_blocks[i].len();

            // Inject the extra padding size entirely into the LAST block's header
            if i == length - 1 {
                comp_len += padding_required;
            }

            write_u32(&mut out, 16 + i * 8, comp_len as u32);
            write_u32(&mut out, 16 + i * 8 + 4, uncompressed_sizes[i]);

            out[offset..offset + compressed_blocks[i].len()].copy_from_slice(&compressed_blocks[i]);
            offset += compressed_blocks[i].len();
        }

        // The remaining zeroes at the end of 'out' natively serve as our safe padding
        Ok(out)
    }

    pub fn recomp_chunk(dec_bytes: &[u8], blk_sz: usize) -> Result<Vec<u8>, UpkError> {
        recomp_chunk_safely_padded(dec_bytes, blk_sz, None)
    }
}

pub mod patcher {
    use super::upk::{self, UPK_MAGIC};
    use std::collections::HashMap;
    use std::fs;
    use std::path::Path;

    pub fn recomp_chunk_inplace(
        van_bytes: &[u8],
        chunk_pos: usize,
        dec_new: &[u8],
        blk_sz: usize,
        modified_range: (usize, usize),
    ) -> (Option<Vec<u8>>, usize, bool) {
        let unc_tot = u32::from_le_bytes(
            van_bytes[chunk_pos + 12..chunk_pos + 16]
                .try_into()
                .unwrap(),
        ) as usize;
        let length = (unc_tot + blk_sz - 1) / blk_sz;
        let orig_header_end = chunk_pos + 16 + length * 8;

        let mut van_comp_sizes = Vec::with_capacity(length);
        for i in 0..length {
            van_comp_sizes.push(u32::from_le_bytes(
                van_bytes[chunk_pos + 16 + i * 8..chunk_pos + 20 + i * 8]
                    .try_into()
                    .unwrap(),
            ) as usize);
        }

        let orig_comp_total: usize = van_comp_sizes.iter().sum();
        let chunk_max_bytes = orig_header_end - chunk_pos + orig_comp_total;

        let (mod_s, mod_e) = modified_range;
        let mut new_comp_sizes = van_comp_sizes.clone();
        let mut out_arrays = Vec::with_capacity(length);

        let mut read_offset = orig_header_end;

        for i in 0..length {
            let num5 = i * blk_sz;
            let num6 = (num5 + blk_sz).min(unc_tot);

            if num6 > mod_s && num5 < mod_e {
                let slice = &dec_new[num5..num6];
                let comp = upk::compress_sub(slice, van_comp_sizes[i]).unwrap_or_else(|_| vec![]);
                new_comp_sizes[i] = comp.len();
                out_arrays.push(comp);
            } else {
                let slice = van_bytes[read_offset..read_offset + van_comp_sizes[i]].to_vec();
                out_arrays.push(slice);
            }
            read_offset += van_comp_sizes[i];
        }

        let mut total_new_comp: usize = new_comp_sizes.iter().sum();
        let mut header = vec![0u8; 16 + length * 8];

        // Securely inject padding into the last block header just like recomp_chunk_safely_padded
        let mut padding_required = 0;
        let current_size = orig_header_end - chunk_pos + total_new_comp;
        if current_size < chunk_max_bytes {
            padding_required = chunk_max_bytes - current_size;
            total_new_comp += padding_required;
        } else if current_size > chunk_max_bytes {
            return (None, chunk_max_bytes, false);
        }

        header[0..4].copy_from_slice(&UPK_MAGIC.to_le_bytes());
        header[4..8].copy_from_slice(&(blk_sz as u32).to_le_bytes());
        header[8..12].copy_from_slice(&(total_new_comp as u32).to_le_bytes());
        header[12..16].copy_from_slice(&(unc_tot as u32).to_le_bytes());

        for i in 0..length {
            let mut comp_len = new_comp_sizes[i];
            if i == length - 1 {
                comp_len += padding_required;
            }

            header[16 + i * 8..20 + i * 8].copy_from_slice(&(comp_len as u32).to_le_bytes());
            let unc_size = blk_sz.min(unc_tot - i * blk_sz) as u32;
            header[20 + i * 8..24 + i * 8].copy_from_slice(&unc_size.to_le_bytes());
        }

        let mut new_chunk = header;
        for arr in out_arrays {
            new_chunk.extend_from_slice(&arr);
        }

        // Pad the physical array with zeroes, Engine reads it as the compressed stream tail
        new_chunk.resize(chunk_max_bytes, 0);
        (Some(new_chunk), chunk_max_bytes, true)
    }
}

pub mod tfc {
    use super::upk::{self, UPK_MAGIC, UpkError};
    use std::fs::File;
    use std::io::{Read, Seek, SeekFrom};

    const TFC_CHUNK_SIZE: usize = 131072; // 0x020000

    #[inline]
    fn write_u32(buf: &mut [u8], off: usize, val: u32) {
        buf[off..off + 4].copy_from_slice(&val.to_le_bytes());
    }

    pub fn make_tfc_entry(dxt_raw: &[u8]) -> Result<Vec<u8>, UpkError> {
        let length = 1.max((dxt_raw.len() + TFC_CHUNK_SIZE - 1) / TFC_CHUNK_SIZE);
        let mut uncompressed_blocks = Vec::with_capacity(length);
        let mut compressed_blocks = Vec::with_capacity(length);

        for i in 0..length {
            let start = i * TFC_CHUNK_SIZE;
            let end = (start + TFC_CHUNK_SIZE).min(dxt_raw.len());
            let slice = &dxt_raw[start..end];

            uncompressed_blocks.push(slice);
            compressed_blocks.push(upk::zlib_compress(slice, 9)?);
        }

        let total_comp: usize = compressed_blocks.iter().map(|b| b.len()).sum();
        let header_len = 16 + length * 8;
        let mut out = vec![0u8; header_len + total_comp];

        write_u32(&mut out, 0, UPK_MAGIC);
        write_u32(&mut out, 4, TFC_CHUNK_SIZE as u32);
        write_u32(&mut out, 8, total_comp as u32);
        write_u32(&mut out, 12, dxt_raw.len() as u32);

        let mut offset = header_len;
        for i in 0..length {
            let comp = &compressed_blocks[i];
            write_u32(&mut out, 16 + i * 8, comp.len() as u32);
            write_u32(
                &mut out,
                16 + i * 8 + 4,
                uncompressed_blocks[i].len() as u32,
            );

            out[offset..offset + comp.len()].copy_from_slice(comp);
            offset += comp.len();
        }

        Ok(out)
    }

    pub fn tfc_compress(
        dxt_raw: &[u8],
        slot_size: usize,
        tail_path: Option<&str>,
        tail_offset: u64,
    ) -> Option<Vec<u8>> {
        let length = 1.max((dxt_raw.len() + TFC_CHUNK_SIZE - 1) / TFC_CHUNK_SIZE);
        let mut uncompressed_blocks = Vec::with_capacity(length);

        for i in 0..length {
            let start = i * TFC_CHUNK_SIZE;
            let end = (start + TFC_CHUNK_SIZE).min(dxt_raw.len());
            uncompressed_blocks.push(&dxt_raw[start..end]);
        }

        let header_len = 16 + length * 8;
        if slot_size <= header_len {
            return None;
        }
        let target_comp_size = slot_size - header_len;

        let compressed_blocks: Vec<Vec<u8>> = uncompressed_blocks
            .iter()
            .map(|&r| upk::zlib_compress(r, 9).unwrap_or_default())
            .collect();

        // Level 9 is the smallest candidate and therefore the one most likely
        // to fit a fixed TFC slot. The old port recompressed the same payload
        // at every level from 8 through 1 after it already fit, solely to fill
        // more of the slot before padding. UE ignores that padding, so those
        // eight additional full-image compression passes were unnecessary.
        let total_comp: usize = compressed_blocks.iter().map(|c| c.len()).sum();

        if total_comp > target_comp_size {
            return None;
        }

        let mut out = vec![0u8; header_len];
        write_u32(&mut out, 0, UPK_MAGIC);
        write_u32(&mut out, 4, TFC_CHUNK_SIZE as u32);
        write_u32(&mut out, 8, total_comp as u32);
        write_u32(&mut out, 12, dxt_raw.len() as u32);

        for i in 0..length {
            let comp = &compressed_blocks[i];
            write_u32(&mut out, 16 + i * 8, comp.len() as u32);
            write_u32(
                &mut out,
                16 + i * 8 + 4,
                uncompressed_blocks[i].len() as u32,
            );
        }

        for comp in compressed_blocks {
            out.extend_from_slice(&comp);
        }

        let remaining_space = slot_size - out.len();
        if remaining_space == 0 {
            return Some(out);
        }

        let mut tail_padding = vec![0u8; remaining_space];
        if let Some(path) = tail_path {
            if let Ok(mut file) = File::open(path) {
                let required_offset = tail_offset + out.len() as u64;
                if let Ok(file_len) = file.metadata().map(|m| m.len()) {
                    if file_len > required_offset {
                        let _ = file.seek(SeekFrom::Start(required_offset));
                        let _ = file.read_exact(&mut tail_padding);
                    }
                }
            }
        }

        out.extend_from_slice(&tail_padding);
        Some(out)
    }
}

pub mod mutators {
    use image::imageops::FilterType;
    use std::fs::{self, OpenOptions};
    use std::io::{Seek, SeekFrom, Write};
    use std::path::Path;

    use super::dxt;
    use super::patcher;
    use super::tfc;
    use super::upk;

    const MB_BLOCK6_POS: usize = 5491748;
    const MB_TFC4_VAN_SIZE: u64 = 3330012825;

    // Mask / Specular / Emissive Maps Info
    const MB_D2K_NEW_OIFS: [u64; 5] = [2899130559, 2897554837, 2897104997, 2896974400, 2896936831];
    const MB_D2K_OIF_OFFS: [usize; 5] = [484906, 484934, 484962, 484990, 485018];
    const MB_D2K_MIPS: [(usize, usize); 7] = [
        (64, 485046),
        (32, 489162),
        (16, 490206),
        (8, 490482),
        (4, 490566),
        (2, 490602),
        (1, 490638),
    ];

    // Normal / Bump Maps Info
    const MB_N2K_TFC_WRITES: [(u64, usize, usize); 5] = [
        (2894783573, 2142019, 2048),
        (2894120085, 663488, 1024),
        (2893924370, 195715, 512),
        (2893869976, 54394, 256),
        (2893855016, 14960, 128),
    ];
    const MB_N2K_MIPS: [(usize, usize); 7] = [
        (64, 491323),
        (32, 495439),
        (16, 496483),
        (8, 496759),
        (4, 496843),
        (2, 496879),
        (1, 496915),
    ];

    // Texture Format Overrides & Fixes
    const MB_B7_PF_OFF: usize = 484734;
    const MB_B7_PF_BC7: [u8; 4] = [167, 4, 0, 0];
    const MB_B7_PF_DXT5: [u8; 4] = [121, 2, 0, 0];
    const MB_GEO_FIXES: [(usize, u32, u32); 3] = [
        (230250, 1654, 880),
        (230366, 1655, 881),
        (484457, 1652, 846),
    ];

    const FLAT_NORMAL_BC7_BLOCK: [u8; 16] = [
        2, 211, 63, 253, 211, 63, 253, 255, 255, 255, 43, 73, 146, 36, 73, 146,
    ];

    fn write_u32(buf: &mut [u8], off: usize, val: u32) {
        buf[off..off + 4].copy_from_slice(&val.to_le_bytes());
    }

    fn write_u64(buf: &mut [u8], off: usize, val: u64) {
        buf[off..off + 8].copy_from_slice(&val.to_le_bytes());
    }

    fn read_u32(buf: &[u8], off: usize) -> u32 {
        u32::from_le_bytes(buf[off..off + 4].try_into().unwrap())
    }

    fn flat_normal_bc7(w: usize) -> Vec<u8> {
        let num_blocks = 1.max(w / 4) * 1.max(w / 4);
        let mut out = vec![0u8; num_blocks * 16];
        for i in 0..num_blocks {
            out[i * 16..(i + 1) * 16].copy_from_slice(&FLAT_NORMAL_BC7_BLOCK);
        }
        out
    }

    pub fn patch_mutators(
        game_path: &str,
        game_dir: &str,
        backup_dir: &str,
        png_bytes: &[u8],
    ) -> Result<String, String> {
        let tfc4_path = Path::new(game_dir).join("Textures4.tfc");
        let tfc3_path = Path::new(game_dir).join("Textures3.tfc");

        if !tfc4_path.exists() {
            return Ok("skip:textures4_missing".to_string());
        }

        std::fs::create_dir_all(backup_dir)
            .map_err(|e| format!("Failed to create backup dir: {}", e))?;

        let original_name = Path::new(game_path).file_name().unwrap().to_string_lossy();
        let bak_filename = format!("{}.bak", original_name);
        let bak_path = Path::new(backup_dir).join(bak_filename);

        if !bak_path.exists() {
            fs::copy(game_path, &bak_path).map_err(|e| format!("Failed to copy backup: {}", e))?;
        }
        let num_array1 =
            fs::read(&bak_path).map_err(|e| format!("Failed to read backup: {}", e))?;

        // Extract Block 6 (Diffuse)
        let (dec1, num1_b6, num2_end_b6) = upk::decomp_chunk_at(&num_array1, MB_BLOCK6_POS)
            .map_err(|_| "error:block6_decomp_failed")?;

        let mut num_array2 = dec1.clone();

        // 1. Process diffuse image and generate DXT1 Mips
        let src_img = dxt::composite_over_white(
            image::load_from_memory(png_bytes)
                .map_err(|e| format!("Failed to parse image bytes: {}", e))?
                .to_rgba8(),
        );

        let img_2048 = image::imageops::resize(&src_img, 2048, 2048, FilterType::CatmullRom);
        let img_1024 = image::imageops::resize(&src_img, 1024, 1024, FilterType::CatmullRom);
        let img_512 = image::imageops::resize(&src_img, 512, 512, FilterType::CatmullRom);
        let img_256 = image::imageops::resize(&src_img, 256, 256, FilterType::CatmullRom);
        let img_128 = image::imageops::resize(&src_img, 128, 128, FilterType::CatmullRom);

        let dxt1_1 = dxt::image_to_dxt1(&img_2048, 2048, 2048);
        let dxt1_2 = dxt::image_to_dxt1(&img_1024, 1024, 1024);
        let dxt1_3 = dxt::image_to_dxt1(&img_512, 512, 512);
        let dxt1_4 = dxt::image_to_dxt1(&img_256, 256, 256);
        let dxt1_5 = dxt::image_to_dxt1(&img_128, 128, 128);

        // Splice diffuse mips into Block 6
        let mips = [
            (
                64,
                dxt::image_to_dxt1(
                    &image::imageops::resize(&src_img, 64, 64, FilterType::CatmullRom),
                    64,
                    64,
                ),
                644709,
            ),
            (
                32,
                dxt::image_to_dxt1(
                    &image::imageops::resize(&src_img, 32, 32, FilterType::CatmullRom),
                    32,
                    32,
                ),
                646777,
            ),
            (
                16,
                dxt::image_to_dxt1(
                    &image::imageops::resize(&src_img, 16, 16, FilterType::CatmullRom),
                    16,
                    16,
                ),
                647309,
            ),
            (
                8,
                dxt::image_to_dxt1(
                    &image::imageops::resize(&src_img, 8, 8, FilterType::CatmullRom),
                    8,
                    8,
                ),
                647457,
            ),
            (
                4,
                dxt::image_to_dxt1(
                    &image::imageops::resize(&src_img, 4, 4, FilterType::CatmullRom),
                    4,
                    4,
                ),
                647509,
            ),
        ];

        for (_, mip_data, offset) in mips {
            num_array2[offset..offset + mip_data.len()].copy_from_slice(&mip_data);
        }

        // 2. Diffuse (Textures4.tfc) Appending - ALL 5 upper mips to fix distance swapping
        let num_array3 = tfc::make_tfc_entry(&dxt1_1).map_err(|e| format!("{:?}", e))?;
        let num_array4 = tfc::make_tfc_entry(&dxt1_2).map_err(|e| format!("{:?}", e))?;
        let num_array5 = tfc::make_tfc_entry(&dxt1_3).map_err(|e| format!("{:?}", e))?;
        let num_array6 = tfc::make_tfc_entry(&dxt1_4).map_err(|e| format!("{:?}", e))?;
        let num_array7 = tfc::make_tfc_entry(&dxt1_5).map_err(|e| format!("{:?}", e))?;

        let val1 = MB_TFC4_VAN_SIZE;
        let val2 = val1 + num_array3.len() as u64;
        let val3 = val2 + num_array4.len() as u64;
        let val4 = val3 + num_array5.len() as u64;
        let val5 = val4 + num_array6.len() as u64;
        let total_tfc_len = val5 + num_array7.len() as u64;

        if fs::metadata(&tfc4_path).map_err(|e| e.to_string())?.len() < MB_TFC4_VAN_SIZE {
            return Err("error:textures4_smaller_than_vanilla".to_string());
        }

        let mut tfc4_file = OpenOptions::new()
            .write(true)
            .open(&tfc4_path)
            .map_err(|e| e.to_string())?;
        tfc4_file
            .set_len(total_tfc_len)
            .map_err(|e| e.to_string())?;

        tfc4_file
            .seek(SeekFrom::Start(val1))
            .map_err(|e| e.to_string())?;
        tfc4_file
            .write_all(&num_array3)
            .map_err(|e| e.to_string())?;
        tfc4_file
            .write_all(&num_array4)
            .map_err(|e| e.to_string())?;
        tfc4_file
            .write_all(&num_array5)
            .map_err(|e| e.to_string())?;
        tfc4_file
            .write_all(&num_array6)
            .map_err(|e| e.to_string())?;
        tfc4_file
            .write_all(&num_array7)
            .map_err(|e| e.to_string())?;

        write_u32(&mut num_array2, 644565, num_array3.len() as u32);
        write_u64(&mut num_array2, 644569, val1);
        write_u32(&mut num_array2, 644593, num_array4.len() as u32);
        write_u64(&mut num_array2, 644597, val2);
        write_u32(&mut num_array2, 644621, num_array5.len() as u32);
        write_u64(&mut num_array2, 644625, val3);
        write_u32(&mut num_array2, 644649, num_array6.len() as u32);
        write_u64(&mut num_array2, 644653, val4);
        write_u32(&mut num_array2, 644677, num_array7.len() as u32);
        write_u64(&mut num_array2, 644681, val5);

        // 3. Process Block 7 (Masks / Specular / Normals)
        let mut chunk7_opt = None;
        let mut num9_end_b7 = num2_end_b6;
        let mut old_sz1_b7;

        if let Ok((dec2, num10_b7_sz, chunk_end_b7)) =
            upk::decomp_chunk_at(&num_array1, num2_end_b6)
        {
            let mut num_array7_chunk = dec2.clone();
            num9_end_b7 = chunk_end_b7;
            old_sz1_b7 = num9_end_b7 - num2_end_b6;

            if num_array7_chunk[MB_B7_PF_OFF..MB_B7_PF_OFF + 4] == MB_B7_PF_BC7 {
                num_array7_chunk[MB_B7_PF_OFF..MB_B7_PF_OFF + 4].copy_from_slice(&MB_B7_PF_DXT5);
            }

            for i in 0..5 {
                write_u64(
                    &mut num_array7_chunk,
                    MB_D2K_OIF_OFFS[i],
                    MB_D2K_NEW_OIFS[i],
                );
            }

            for (w, offset) in MB_D2K_MIPS {
                let resized =
                    image::imageops::resize(&src_img, w as u32, w as u32, FilterType::CatmullRom);
                let dxt5_data = dxt::image_to_dxt5(&resized, w, w);
                num_array7_chunk[offset..offset + dxt5_data.len()].copy_from_slice(&dxt5_data);
            }

            if tfc3_path.exists() {
                let mut tfc3_file = OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(&tfc3_path)
                    .map_err(|e| e.to_string())?;

                let mask_entry = tfc::make_tfc_entry(&dxt::image_to_dxt5(&img_2048, 2048, 2048))
                    .map_err(|e| format!("{:?}", e))?;
                tfc3_file
                    .seek(SeekFrom::Start(MB_D2K_NEW_OIFS[0]))
                    .map_err(|e| e.to_string())?;
                tfc3_file
                    .write_all(&mask_entry)
                    .map_err(|e| e.to_string())?;

                for (w, offset) in MB_N2K_MIPS {
                    let normal_data = flat_normal_bc7(w);
                    num_array7_chunk[offset..offset + normal_data.len()]
                        .copy_from_slice(&normal_data);
                }

                for &(oif, slot_size, w) in &MB_N2K_TFC_WRITES {
                    let dxt_raw = flat_normal_bc7(w);
                    let payload = tfc::tfc_compress(
                        &dxt_raw,
                        slot_size,
                        Some(&tfc3_path.to_string_lossy()),
                        oif,
                    )
                    .unwrap_or_else(|| {
                        let mut fallback = tfc::make_tfc_entry(&dxt_raw).unwrap();
                        fallback.resize(slot_size, 0);
                        fallback
                    });

                    tfc3_file
                        .seek(SeekFrom::Start(oif))
                        .map_err(|e| e.to_string())?;
                    tfc3_file
                        .write_all(&payload[..slot_size])
                        .map_err(|e| e.to_string())?;
                }
            }

            for &(offset, van_idx, new_idx) in &MB_GEO_FIXES {
                if read_u32(&num_array7_chunk, offset) == van_idx {
                    write_u32(&mut num_array7_chunk, offset, new_idx);
                }
            }

            let modified_range2 = (230250, 496931);
            let (new_chunk7_opt, _, ok7) = patcher::recomp_chunk_inplace(
                &num_array1,
                num2_end_b6,
                &num_array7_chunk,
                num10_b7_sz as usize,
                modified_range2,
            );

            let new_chunk7 = if ok7 && new_chunk7_opt.is_some() {
                new_chunk7_opt.unwrap()
            } else {
                match upk::recomp_chunk_safely_padded(
                    &num_array7_chunk,
                    num10_b7_sz as usize,
                    Some(old_sz1_b7),
                ) {
                    Ok(c) => c,
                    Err(_) => return Err("error:oversize_block7".into()),
                }
            };
            chunk7_opt = Some(new_chunk7);
        }

        // 4. Recompress Block 6 & Save
        let old_sz2_b6 = num2_end_b6 - MB_BLOCK6_POS;
        let modified_range1 = (644565, 647517);

        let (new_chunk6_opt, _orig_size, ok6) = patcher::recomp_chunk_inplace(
            &num_array1,
            MB_BLOCK6_POS,
            &num_array2,
            num1_b6 as usize,
            modified_range1,
        );

        let new_chunk6 = if ok6 && new_chunk6_opt.is_some() {
            new_chunk6_opt.unwrap()
        } else {
            match upk::recomp_chunk_safely_padded(&num_array2, num1_b6 as usize, Some(old_sz2_b6)) {
                Ok(c) => c,
                Err(_) => return Err("error:oversize_block6".into()),
            }
        };

        let mut final_file = Vec::with_capacity(num_array1.len());
        final_file.extend_from_slice(&num_array1[..MB_BLOCK6_POS]);
        final_file.extend_from_slice(&new_chunk6);
        if let Some(c7) = &chunk7_opt {
            final_file.extend_from_slice(c7);
        }
        final_file.extend_from_slice(&num_array1[num9_end_b7..]);

        fs::write(game_path, &final_file).map_err(|e| e.to_string())?;

        Ok("ok".to_string())
    }
}

pub mod standard_ball {
    use super::{dxt, tfc};
    use image::{Rgba, RgbaImage, imageops::FilterType};
    use std::collections::HashMap;
    use std::fs::{self, OpenOptions};
    use std::io::{Read, Seek, SeekFrom, Write};
    use std::path::Path;

    const TFC_ENTRIES: [(&str, u64, usize, u32); 14] = [
        ("Textures2.tfc", 856931869, 686345, 2048),
        ("Textures2.tfc", 856758008, 173861, 1024),
        ("Textures2.tfc", 63510294, 47340, 512),
        ("Textures2.tfc", 63496818, 13476, 256),
        ("Textures2.tfc", 63492915, 3903, 128),
        ("Textures2.tfc", 3164923583, 178040, 1024),
        ("Textures2.tfc", 3165101623, 50434, 512),
        ("Textures2.tfc", 3165152057, 14086, 256),
        ("Textures2.tfc", 3165166143, 4132, 128),
        ("Textures4.tfc", 2792441005, 339187, 2048),
        ("Textures4.tfc", 2792328137, 112868, 1024),
        ("Textures4.tfc", 2792289507, 38630, 512),
        ("Textures4.tfc", 2792275360, 14147, 256),
        ("Textures4.tfc", 2792270623, 4737, 128),
    ];

    pub fn is_ball_tfc_backup(name: &str) -> bool {
        TFC_ENTRIES.iter().any(|(tfc_name, offset, _, _)| {
            name.eq_ignore_ascii_case(&format!("{tfc_name}_{offset}.bin"))
        })
    }

    // Translated directly from C#'s QuantizeDxt1 math
    fn quantize_image(img: &RgbaImage, n_colors: u32) -> RgbaImage {
        let mut new_img = img.clone();
        let steps = (n_colors as f64).cbrt().ceil().max(2.0) as u8 - 1;
        let step_size = 255.0 / steps as f64;

        for pixel in new_img.pixels_mut() {
            let r = ((pixel[0] as f64 / step_size).round() * step_size).min(255.0) as u8;
            let g = ((pixel[1] as f64 / step_size).round() * step_size).min(255.0) as u8;
            let b = ((pixel[2] as f64 / step_size).round() * step_size).min(255.0) as u8;
            *pixel = Rgba([r, g, b, pixel[3]]);
        }
        new_img
    }

    // Translated from C#'s Average Color fallback
    fn average_color(img: &RgbaImage) -> (u8, u8, u8) {
        let mut r_sum = 0u64;
        let mut g_sum = 0u64;
        let mut b_sum = 0u64;
        let mut count = 0u64;

        for pixel in img.pixels() {
            if pixel[0].max(pixel[1]).max(pixel[2]) >= 15 {
                r_sum += pixel[0] as u64;
                g_sum += pixel[1] as u64;
                b_sum += pixel[2] as u64;
                count += 1;
            }
        }

        if count == 0 {
            return (128, 128, 128);
        }
        (
            (r_sum / count) as u8,
            (g_sum / count) as u8,
            (b_sum / count) as u8,
        )
    }

    pub fn patch_standard_tfcs(
        game_dir: &str,
        backup_dir: &str,
        png_bytes: &[u8],
    ) -> Result<(), String> {
        let src_img = dxt::composite_over_white(
            image::load_from_memory(png_bytes)
                .map_err(|e| format!("Failed to parse image bytes: {}", e))?
                .to_rgba8(),
        );
        let average = average_color(&src_img);
        let mut resized_cache: HashMap<u32, RgbaImage> = HashMap::new();
        let mut dxt_cache: HashMap<u32, Vec<u8>> = HashMap::new();
        for width in [2048, 1024, 512, 256, 128] {
            let resized = image::imageops::resize(&src_img, width, width, FilterType::CatmullRom);
            dxt_cache.insert(
                width,
                dxt::image_to_dxt1(&resized, width as usize, width as usize),
            );
            resized_cache.insert(width, resized);
        }
        let mut quantized_dxt_cache: HashMap<(u32, u32), Vec<u8>> = HashMap::new();
        let mut flat_dxt_cache: HashMap<u32, Vec<u8>> = HashMap::new();

        for (tfc_name, offset, slot_size, w) in TFC_ENTRIES {
            let tfc_path = Path::new(game_dir).join(tfc_name);
            if !tfc_path.exists() {
                continue;
            }

            let backup_bin = Path::new(backup_dir).join(format!("{}_{}.bin", tfc_name, offset));
            let mut tfc_file = OpenOptions::new()
                .read(true)
                .write(true)
                .open(&tfc_path)
                .map_err(|e| e.to_string())?;

            if !backup_bin.exists() {
                let mut old_data = vec![0u8; slot_size];
                if tfc_file.seek(SeekFrom::Start(offset)).is_ok() {
                    if tfc_file.read_exact(&mut old_data).is_ok() {
                        let _ = fs::write(&backup_bin, &old_data);
                    }
                }
            }

            let resized = resized_cache
                .get(&w)
                .expect("standard ball width was prepared");
            let dxt_raw = dxt_cache.get(&w).expect("standard ball DXT was prepared");

            // Initial Attempt
            let mut payload_opt = tfc::tfc_compress(
                dxt_raw,
                slot_size,
                Some(&tfc_path.to_string_lossy()),
                offset,
            );

            // Fallback 1: C# Color Quantization Loop
            if payload_opt.is_none() {
                let color_steps = [128, 64, 32, 16, 8, 4];
                for &colors in &color_steps {
                    let q_dxt = quantized_dxt_cache.entry((w, colors)).or_insert_with(|| {
                        let quantized = quantize_image(resized, colors);
                        dxt::image_to_dxt1(&quantized, w as usize, w as usize)
                    });
                    payload_opt = tfc::tfc_compress(
                        q_dxt,
                        slot_size,
                        Some(&tfc_path.to_string_lossy()),
                        offset,
                    );
                    if payload_opt.is_some() {
                        break;
                    }
                }
            }

            // Fallback 2: C# Flat Average Color
            if payload_opt.is_none() {
                let f_dxt = flat_dxt_cache.entry(w).or_insert_with(|| {
                    let mut flat_img = RgbaImage::new(w, w);
                    for pixel in flat_img.pixels_mut() {
                        *pixel = Rgba([average.0, average.1, average.2, 255]);
                    }
                    dxt::image_to_dxt1(&flat_img, w as usize, w as usize)
                });
                payload_opt =
                    tfc::tfc_compress(f_dxt, slot_size, Some(&tfc_path.to_string_lossy()), offset);
            }

            if let Some(payload) = payload_opt {
                if tfc_file.seek(SeekFrom::Start(offset)).is_ok() {
                    let _ = tfc_file.write_all(&payload);
                }
            }
        }

        Ok(())
    }
}

pub mod gameinfo {
    use super::{dxt, patcher, upk};
    use image::imageops::FilterType;
    use std::collections::HashMap;
    use std::fs;
    use std::path::Path;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    const VANILLA_THUMB_SIG: [u8; 8] = [0, 0, 76, 107, 0, 0, 0, 128];
    const VAN_PRE_TRAIL: [u8; 116] = [
        0x25, 0xa7, 0x02, 0x00, 0xf8, 0x16, 0x11, 0x33, 0x00, 0x00, 0x00, 0x00, 0x00, 0x04, 0x00,
        0x00, 0x00, 0x04, 0x00, 0x00, 0x03, 0x00, 0x01, 0x00, 0x00, 0x00, 0x02, 0x00, 0xec, 0xb8,
        0x00, 0x00, 0x16, 0x17, 0xc9, 0x03, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00,
        0x02, 0x00, 0x00, 0x03, 0x00, 0x01, 0x00, 0x00, 0x80, 0x00, 0x00, 0xa4, 0x34, 0x00, 0x00,
        0x72, 0xe2, 0xc8, 0x03, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x00,
        0x00, 0x03, 0x00, 0x01, 0x00, 0x00, 0x20, 0x00, 0x00, 0x3f, 0x0f, 0x00, 0x00, 0x33, 0xd3,
        0xc8, 0x03, 0x00, 0x00, 0x00, 0x00, 0x80, 0x00, 0x00, 0x00, 0x80, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x01, 0x00, 0x00, 0x08, 0x00, 0x00, 0x00, 0x08, 0x00, 0x00,
    ];
    const PATCHED_PRE_TRAIL: [u8; 116] = [
        0x78, 0xb7, 0x02, 0x00, 0xbf, 0xe6, 0xa4, 0xbc, 0x00, 0x00, 0x00, 0x00, 0x00, 0x04, 0x00,
        0x00, 0x00, 0x04, 0x00, 0x00, 0x03, 0x00, 0x01, 0x00, 0x00, 0x00, 0x02, 0x00, 0x02, 0xc5,
        0x00, 0x00, 0x37, 0x9e, 0xa7, 0xbc, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00,
        0x02, 0x00, 0x00, 0x03, 0x00, 0x01, 0x00, 0x00, 0x80, 0x00, 0x00, 0x06, 0x37, 0x00, 0x00,
        0x39, 0x63, 0xa8, 0xbc, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x00,
        0x00, 0x03, 0x00, 0x01, 0x00, 0x00, 0x20, 0x00, 0x00, 0x24, 0x10, 0x00, 0x00, 0x3f, 0x9a,
        0xa8, 0xbc, 0x00, 0x00, 0x00, 0x00, 0x80, 0x00, 0x00, 0x00, 0x80, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x01, 0x00, 0x00, 0x08, 0x00, 0x00, 0x00, 0x08, 0x00, 0x00,
    ];

    const PH4D_SPECS: [(usize, usize); 5] = [(64, 2048), (32, 512), (16, 128), (8, 32), (4, 8)];
    const PH4D_INLINE_B: [u8; 4] = [0, 0, 1, 0];

    // Kept in sync with BallPatcher.BallUpks. These packages carry the inline
    // low mips used after the streamed TFC texture drops out with distance.
    const BALL_UPKS: &[&str] = &[
        "beach_night_grs_p.upk",
        "bg_beachnight_grs.upk",
        "bg_eurostadium_dusk.upk",
        "bg_ff_dusk.upk",
        "bg_fni_stadium.upk",
        "bg_mall_day_p.upk",
        "bg_neotokyo_arcade.upk",
        "bg_neotokyo_hax.upk",
        "bg_outlaw_oasis.upk",
        "bg_paname_dusk.upk",
        "bg_park_snowy.upk",
        "bg_utopiasnow.upk",
        "bg_stadium.upk",
        "bg_stadium_10a_p.upk",
        "bg_uf_day_p.upk",
        "bg_uf_night_p.upk",
        "bg_underwater_grs.upk",
        "bg_woods_day_p.upk",
        "bg_woods_night_p.upk",
        "explosion_hug_sf.upk",
        "gameinfo_basketball_sf.upk",
        "gameinfo_breakout_sf.upk",
        "gameinfo_football_sf.upk",
        "gameinfo_fte_sf.upk",
        "gameinfo_godball_sf.upk",
        "gameinfo_heatseekerterritory_sf.upk",
        "gameinfo_hockey_sf.upk",
        "gameinfo_hops_sf.upk",
        "gameinfo_items_sf.upk",
        "gameinfo_magnusfutball_sf.upk",
        "gameinfo_keepup_sf.upk",
        "gameinfo_knockout_sf.upk",
        "gameinfo_ltm_aprilfool_sf.upk",
        "gameinfo_ltm_beachball_sf.upk",
        "gameinfo_ltm_boomerball_sf.upk",
        "gameinfo_ltm_demolition_sf.upk",
        "gameinfo_ltm_dropshotrumble_sf.upk",
        "gameinfo_ltm_eggstra_sf.upk",
        "gameinfo_ltm_gforce_sf.upk",
        "gameinfo_ltm_moonball_sf.upk",
        "gameinfo_ltm_pinball_sf.upk",
        "gameinfo_ltm_speeddemon_sf.upk",
        "gameinfo_ltm_spikerush_sf.upk",
        "gameinfo_ltm_supercube_sf.upk",
        "gameinfo_possession_sf.upk",
        "gameinfo_season_sf.upk",
        "gameinfo_snowdayterritory_sf.upk",
        "gameinfo_soccar_sf.upk",
        "gameinfo_spikedrop_sf.upk",
        "gameinfo_territory_sf.upk",
        "gameinfo_trainingeditor_sf.upk",
        "gameinfo_tutorial_sf.upk",
        "labs_octagon_b2b_02_p.upk",
        "menu_main_p.upk",
        "neotokyo_arcade_p.upk",
        "neotokyo_buildings.upk",
        "neotokyo_buildings_s.upk",
        "neotokyo_hax_p.upk",
        "neotokyo_hax_signs_p.upk",
        "neotokyo_p.upk",
        "neotokyo_standard_p.upk",
        "neotokyo_toon_p.upk",
        "nue_cine_master.upk",
        "paname_dusk_p.upk",
        "tutorialadvanced.upk",
        "tutorialbeginner.upk",
        "utopiastadium_dusk_p.upk",
        "utopiastadium_lux_p.upk",
        "utopiastadium_p.upk",
        "utopiastadium_snow_p.upk",
    ];

    pub fn is_ball_upk(name: &str) -> bool {
        name.eq_ignore_ascii_case("Mutators_Balls_SF.upk")
            || BALL_UPKS
                .iter()
                .any(|candidate| name.eq_ignore_ascii_case(candidate))
    }

    struct PreparedBallTextures {
        dxt64: Vec<u8>,
        ph4d_dxt: HashMap<usize, Vec<u8>>,
        post_trail: Vec<u8>,
    }

    fn prepare_ball_textures(png_bytes: &[u8]) -> Result<PreparedBallTextures, String> {
        let src_img = dxt::composite_over_white(
            image::load_from_memory(png_bytes)
                .map_err(|e| format!("Failed to parse image bytes: {e}"))?
                .to_rgba8(),
        );
        let mip64_img = image::imageops::resize(&src_img, 64, 64, FilterType::CatmullRom);
        let dxt64 = dxt::image_to_dxt1(&mip64_img, 64, 64);
        let decoded64 = dxt::decode_dxt1(&dxt64, 64, 64);
        let average = average_non_black(&decoded64);

        let mip32_img = image::imageops::resize(&decoded64, 32, 32, FilterType::CatmullRom);
        let dxt32 = dxt::image_to_dxt1(&mip32_img, 32, 32);
        let decoded32 = dxt::decode_dxt1(&dxt32, 32, 32);
        let mip16_img = image::imageops::resize(&decoded32, 16, 16, FilterType::CatmullRom);
        let dxt16 = dxt::image_to_dxt1(&mip16_img, 16, 16);
        let decoded16 = dxt::decode_dxt1(&dxt16, 16, 16);
        let mip8_img = image::imageops::resize(&decoded16, 8, 8, FilterType::CatmullRom);
        let dxt8 = dxt::image_to_dxt1(&mip8_img, 8, 8);
        let decoded8 = dxt::decode_dxt1(&dxt8, 8, 8);
        let mip4_img = image::imageops::resize(&decoded8, 4, 4, FilterType::CatmullRom);
        let dxt4 = dxt::image_to_dxt1(&mip4_img, 4, 4);

        let ph4d_dxt = HashMap::from([
            (64, dxt64.clone()),
            (32, dxt32),
            (16, dxt16),
            (8, dxt8),
            (4, dxt4),
        ]);
        Ok(PreparedBallTextures {
            dxt64,
            ph4d_dxt,
            post_trail: generate_post_trail(&decoded64, average),
        })
    }

    fn index_of(haystack: &[u8], needle: &[u8], start: usize) -> Option<usize> {
        if needle.is_empty() || haystack.len() < needle.len() {
            return None;
        }
        for i in start..=haystack.len().saturating_sub(needle.len()) {
            if &haystack[i..i + needle.len()] == needle {
                return Some(i);
            }
        }
        None
    }

    fn find_inline_chains(dec: &[u8]) -> Vec<Vec<(usize, usize, usize)>> {
        let mut chains = Vec::new();
        let mut start = 0;

        while let Some(num3) = index_of(dec, &PH4D_INLINE_B, start) {
            start = num3 + 1;
            if num3 + 20 > dec.len() {
                break;
            }

            let int32_3 = i32::from_le_bytes(dec[num3 + 4..num3 + 8].try_into().unwrap());
            let int32_4 = i32::from_le_bytes(dec[num3 + 8..num3 + 12].try_into().unwrap());

            if int32_3 != 2048 || int32_4 != 2048 {
                continue;
            }

            let num1 = num3 + 12;
            let num2 = num1 + 2048;

            if num2 + 8 > dec.len() {
                continue;
            }

            let int32_1 = i32::from_le_bytes(dec[num2..num2 + 4].try_into().unwrap());
            let int32_2 = i32::from_le_bytes(dec[num2 + 4..num2 + 8].try_into().unwrap());

            if int32_1 != 64 || int32_2 != 64 {
                continue;
            }

            let mut chain = vec![(64, num1, num2)];
            let mut num4 = num2 + 8;

            for &(w, esod) in &PH4D_SPECS[1..] {
                if num4 + 12 <= dec.len() {
                    let int32_5 = i32::from_le_bytes(dec[num4..num4 + 4].try_into().unwrap());
                    let int32_6 =
                        i32::from_le_bytes(dec[num4 + 4..num4 + 8].try_into().unwrap()) as usize;
                    let int32_7 =
                        i32::from_le_bytes(dec[num4 + 8..num4 + 12].try_into().unwrap()) as usize;

                    if int32_5 == 65536 && int32_6 == esod && int32_7 == esod {
                        let num5 = num4 + 12;
                        let num6 = num5 + esod;
                        if num6 + 8 <= dec.len() {
                            chain.push((w, num5, num6));
                            num4 = num6 + 8;
                        }
                    } else {
                        break;
                    }
                } else {
                    break;
                }
            }

            if chain.len() >= 3 {
                chains.push(chain);
            }
        }
        chains
    }

    fn fill_dark_pixels(img: &mut image::RgbaImage, fill: (u8, u8, u8)) {
        for pixel in img.pixels_mut() {
            if pixel[0].max(pixel[1]).max(pixel[2]) < 15 {
                pixel[0] = fill.0;
                pixel[1] = fill.1;
                pixel[2] = fill.2;
            }
        }
    }

    fn average_non_black(img: &image::RgbaImage) -> (u8, u8, u8) {
        let mut totals = [0u64; 3];
        let mut count = 0u64;
        for pixel in img.pixels() {
            if pixel[0].max(pixel[1]).max(pixel[2]) >= 15 {
                totals[0] += pixel[0] as u64;
                totals[1] += pixel[1] as u64;
                totals[2] += pixel[2] as u64;
                count += 1;
            }
        }
        if count == 0 {
            (128, 128, 128)
        } else {
            (
                (totals[0] / count) as u8,
                (totals[1] / count) as u8,
                (totals[2] / count) as u8,
            )
        }
    }

    fn generate_post_trail(img: &image::RgbaImage, fill: (u8, u8, u8)) -> Vec<u8> {
        let mut resized_8 = image::imageops::resize(img, 8, 8, FilterType::CatmullRom);
        fill_dark_pixels(&mut resized_8, fill);
        let dxt_8 = dxt::image_to_dxt1(&resized_8, 8, 8);
        let decoded_8 = dxt::decode_dxt1(&dxt_8, 8, 8);
        let mut resized_4 = image::imageops::resize(&decoded_8, 4, 4, FilterType::CatmullRom);
        fill_dark_pixels(&mut resized_4, fill);
        let dxt_4 = dxt::image_to_dxt1(&resized_4, 4, 4);

        let pt_hdr2: [u8; 20] = [8, 0, 0, 0, 8, 0, 0, 0, 0, 0, 1, 0, 8, 0, 0, 0, 8, 0, 0, 0];
        let pt_hdr3: [u8; 20] = [4, 0, 0, 0, 4, 0, 0, 0, 0, 0, 1, 0, 8, 0, 0, 0, 8, 0, 0, 0];
        let pt_hdr4: [u8; 20] = [4, 0, 0, 0, 4, 0, 0, 0, 0, 0, 1, 0, 8, 0, 0, 0, 8, 0, 0, 0];

        let mut out = Vec::with_capacity(116);
        out.extend_from_slice(&32u32.to_le_bytes());
        out.extend_from_slice(&dxt_8);
        out.extend_from_slice(&pt_hdr2);
        out.extend_from_slice(&dxt_4);
        out.extend_from_slice(&pt_hdr3);
        out.extend_from_slice(&dxt_4);
        out.extend_from_slice(&pt_hdr4);
        out.extend_from_slice(&dxt_4[..4]);
        out
    }

    fn patch_ball_upk(
        game_dir: &str,
        backup_dir: &str,
        textures: &PreparedBallTextures,
        target_name: &str,
    ) -> Result<bool, String> {
        let target_path = Path::new(game_dir).join(target_name);
        if !target_path.exists() {
            return Ok(false);
        }

        let bak_path = Path::new(backup_dir).join(format!("{target_name}.bak"));
        let created_backup = !bak_path.exists();
        if !bak_path.exists() {
            fs::copy(&target_path, &bak_path).map_err(|e| e.to_string())?;
        }

        let van_bytes = fs::read(&bak_path).map_err(|e| e.to_string())?;

        let mut found = None;
        // Match C# FindSig: the package header stores the first compressed
        // region offset at byte 8, so do not scan the entire header bytewise.
        let mut off = van_bytes
            .get(8..12)
            .map(|bytes| u32::from_le_bytes(bytes.try_into().unwrap()) as usize)
            .filter(|offset| *offset > 16 && *offset < van_bytes.len())
            .unwrap_or(4);
        while off + 16 <= van_bytes.len() {
            if let Ok((dec, blk_sz, end_pos)) = upk::decomp_chunk_at(&van_bytes, off) {
                if let Some(idx) = index_of(&dec, &VANILLA_THUMB_SIG, 0) {
                    found = Some((off, end_pos, idx, dec, blk_sz));
                    break;
                } else if let Some(idx) = index_of(&dec, &VAN_PRE_TRAIL, 0) {
                    found = Some((off, end_pos, idx + 116, dec, blk_sz));
                    break;
                }
                off = end_pos;
            } else {
                off += 1;
            }
        }

        let Some((chunk_pos, chunk_end, local_idx_raw, dec_orig, blk_sz)) = found else {
            // C# treats packages without the ball texture signature as a skip.
            if created_backup {
                let _ = fs::remove_file(&bak_path);
            }
            return Ok(false);
        };

        if local_idx_raw < 116 {
            return Err("Signature too close to chunk boundary".into());
        }

        let mut dec_modified = dec_orig.clone();

        let thumb_data = &dec_orig[local_idx_raw + 2048..local_idx_raw + 2744];
        let mut patched_thumb = Vec::with_capacity(2744);
        patched_thumb.extend_from_slice(&textures.dxt64);
        patched_thumb.extend_from_slice(thumb_data);

        // This metadata describes the replacement thumbnail/mip trail.  The
        // old Rust port accidentally wrote the vanilla descriptor back here,
        // so UE selected the wrong data once the texture streamed to a lower
        // mip at distance.
        dec_modified[local_idx_raw - 116..local_idx_raw].copy_from_slice(&PATCHED_PRE_TRAIL);
        dec_modified[local_idx_raw..local_idx_raw + 2744].copy_from_slice(&patched_thumb);
        dec_modified[local_idx_raw + 2744..local_idx_raw + 2860]
            .copy_from_slice(&textures.post_trail);

        let (new_chunk_opt, _, ok) = patcher::recomp_chunk_inplace(
            &van_bytes,
            chunk_pos,
            &dec_modified,
            blk_sz as usize,
            (local_idx_raw - 116, local_idx_raw + 2860),
        );

        let new_chunk = if ok && new_chunk_opt.is_some() {
            new_chunk_opt.unwrap()
        } else {
            match upk::recomp_chunk_safely_padded(
                &dec_modified,
                blk_sz as usize,
                Some(chunk_end - chunk_pos),
            ) {
                Ok(chunk) => chunk,
                Err(upk::UpkError::OversizedChunk) => return Ok(false),
                Err(error) => return Err(format!("{error:?}")),
            }
        };

        let mut final_file = Vec::with_capacity(van_bytes.len());
        final_file.extend_from_slice(&van_bytes[..chunk_pos]);
        final_file.extend_from_slice(&new_chunk);
        if chunk_pos + new_chunk.len() < chunk_end {
            final_file.resize(chunk_end, 0);
        }
        final_file.extend_from_slice(&van_bytes[chunk_end..]);

        // Match the C# two-pass write. Keeping the lower-mip rewrite separate
        // prevents the wider modified range from making the thumbnail chunk
        // exceed its fixed physical slot. If a secondary mip pass cannot fit,
        // retain the valid thumbnail patch instead of aborting the whole ball.
        if let Ok((dec_after_thumb, _, after_end)) = upk::decomp_chunk_at(&final_file, chunk_pos) {
            let mut dec_with_mips = dec_after_thumb.clone();
            let mut mip_range: Option<(usize, usize)> = None;
            for chain in find_inline_chains(&dec_orig) {
                let Some((_, first_start, first_end)) = chain.first().copied() else {
                    continue;
                };
                let top_changed = first_end <= dec_orig.len()
                    && first_end <= dec_after_thumb.len()
                    && dec_orig[first_start..first_end] != dec_after_thumb[first_start..first_end];
                let lower_still_vanilla = chain.iter().skip(1).any(|&(_, start, end)| {
                    end <= dec_orig.len()
                        && end <= dec_after_thumb.len()
                        && dec_orig[start..end] == dec_after_thumb[start..end]
                });
                if !top_changed || !lower_still_vanilla {
                    continue;
                }
                for (width, start, _) in chain {
                    if let Some(data) = textures.ph4d_dxt.get(&width) {
                        if start + data.len() <= dec_with_mips.len() {
                            dec_with_mips[start..start + data.len()].copy_from_slice(data);
                            mip_range = Some(match mip_range {
                                Some((min, max)) => (min.min(start), max.max(start + data.len())),
                                None => (start, start + data.len()),
                            });
                        }
                    }
                }
            }

            if let Some(range) = mip_range {
                let (mip_chunk, _, fits) = patcher::recomp_chunk_inplace(
                    &final_file,
                    chunk_pos,
                    &dec_with_mips,
                    blk_sz as usize,
                    range,
                );
                if fits {
                    if let Some(mip_chunk) = mip_chunk {
                        final_file[chunk_pos..after_end].copy_from_slice(&mip_chunk);
                    }
                }
            }
        }

        fs::write(&target_path, &final_file).map_err(|e| e.to_string())?;

        Ok(true)
    }

    pub fn patch_ball_upks(
        game_dir: &str,
        backup_dir: &str,
        png_bytes: &[u8],
    ) -> Result<usize, String> {
        // C# prepares this mip chain once before walking BallUpks. Rebuilding
        // it for every package made a single ball apply unnecessarily slow.
        let textures = prepare_ball_textures(png_bytes)?;
        let targets: Vec<&str> = BALL_UPKS
            .iter()
            .copied()
            .filter(|target| Path::new(game_dir).join(target).is_file())
            .collect();
        if targets.is_empty() {
            return Ok(0);
        }

        // Each package and backup has a distinct path. A small worker pool
        // lets decompression/recompression use modern CPUs without creating
        // enough concurrent large buffers to exhaust memory.
        let worker_count = std::thread::available_parallelism()
            .map(usize::from)
            .unwrap_or(1)
            .min(4)
            .min(targets.len());
        let next = AtomicUsize::new(0);
        let patched = AtomicUsize::new(0);
        let failed = AtomicBool::new(false);
        let first_error = Mutex::new(None::<String>);
        std::thread::scope(|scope| {
            for _ in 0..worker_count {
                scope.spawn(|| {
                    while !failed.load(Ordering::Relaxed) {
                        let index = next.fetch_add(1, Ordering::Relaxed);
                        let Some(target) = targets.get(index) else {
                            break;
                        };
                        match patch_ball_upk(game_dir, backup_dir, &textures, target) {
                            Ok(true) => {
                                patched.fetch_add(1, Ordering::Relaxed);
                            }
                            Ok(false) => {}
                            Err(error) => {
                                failed.store(true, Ordering::Relaxed);
                                if let Ok(mut first) = first_error.lock() {
                                    if first.is_none() {
                                        *first = Some(format!("{target}: {error}"));
                                    }
                                }
                            }
                        }
                    }
                });
            }
        });
        if let Some(error) = first_error.into_inner().unwrap_or(None) {
            Err(error)
        } else {
            Ok(patched.load(Ordering::Relaxed))
        }
    }
}
