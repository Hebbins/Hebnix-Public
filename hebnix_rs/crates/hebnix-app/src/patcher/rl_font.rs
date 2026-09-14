// crates/hebnix-app/src/patcher/rl_font.rs
//! rebuilds rl's ui fonts from the player's install so overlay text matches the
//! game. GFX_Fonts_SF.upk holds scaleform movies whose DefineFont3 tags carry
//! vector outlines, not font files, so glyphs become an sfnt in memory.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use aes::Aes256;
use aes::cipher::generic_array::GenericArray;
use aes::cipher::{BlockDecrypt, KeyInit};

use crate::patcher::patch_core::upk;
use crate::patcher::upk_keys::DEFAULT_UPK_KEY;

// DefineFont3 coordinates use a 20480 em, truetype gets the usual 2048
const SWF_EM: i32 = 20480;
const UPEM: i32 = 2048;
const SCALE: i32 = SWF_EM / UPEM;

pub struct RlFont {
    pub id: &'static str, // what plugins ask for
    pub face: String,     // psyonix's name, for logs
    pub ttf: Vec<u8>,
}

fn family_id(face: &str) -> Option<&'static str> {
    Some(match face {
        "Dashboard Numbers Wide" => "rl-digits",
        "Bourgeois Medium" => "rl-header",
        "Bourgeois Thin" => "rl-header-thin",
        "Arial Narrow" => "rl-body",
        "Arial Narrow Bold" => "rl-body-bold",
        _ => return None,
    })
}

/// every ui font in the install at rl_dir
pub fn extract(rl_dir: &Path) -> Result<Vec<RlFont>, String> {
    let path = rl_dir
        .join("TAGame")
        .join("CookedPCConsole")
        .join("GFX_Fonts_SF.upk");
    let raw = std::fs::read(&path).map_err(|e| format!("cant read {}: {e}", path.display()))?;
    let logical = load_package(&raw)?;
    let mut out: Vec<RlFont> = Vec::new();
    for movie in find_movies(&logical) {
        for font in read_fonts(movie) {
            let Some(id) = family_id(&font.name) else {
                continue;
            };
            let Some(family) = dwrite_family(id) else {
                continue;
            };
            // the package ships several copies of each locale movie
            if out.iter().any(|f| f.id == id) {
                continue;
            }
            let ttf = build_ttf(&font, family);
            out.push(RlFont {
                id,
                face: font.name,
                ttf,
            });
        }
    }
    if out.is_empty() {
        return Err("no DefineFont3 tags found in GFX_Fonts_SF.upk".into());
    }
    for font in &out {
        tracing::info!("rl font {} rebuilt from {}", font.id, font.face);
    }
    Ok(out)
}

static INSTALL: Mutex<Option<PathBuf>> = Mutex::new(None);

/// drops the rebuilt faces when the path moves, safe to call every frame
pub fn set_install_dir(dir: &Path) {
    let mut slot = lock(&INSTALL);
    if slot.as_deref() != Some(dir) {
        tracing::info!("rl fonts will load from {}", dir.display());
        *slot = Some(dir.to_path_buf());
        *lock(&CACHE) = None;
        GENERATION.fetch_add(1, Ordering::SeqCst);
    }
}

/// bumps when the faces change, the overlay's collection rebuilds on it
pub fn generation() -> u64 {
    GENERATION.load(Ordering::SeqCst)
}

static GENERATION: AtomicU64 = AtomicU64::new(0);

static CACHE: Mutex<Option<(PathBuf, Arc<Vec<RlFont>>)>> = Mutex::new(None);

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn cached() -> Arc<Vec<RlFont>> {
    // INSTALL before CACHE, the same order set_install_dir takes them
    let install = lock(&INSTALL);
    let Some(dir) = install.as_ref() else {
        return Arc::clone(&EMPTY);
    };
    let mut slot = lock(&CACHE);
    if let Some((path, fonts)) = slot.as_ref() {
        if path == dir {
            return Arc::clone(fonts);
        }
    }
    let fonts = Arc::new(match extract(dir) {
        Ok(fonts) => fonts,
        Err(error) => {
            tracing::warn!("rl fonts unavailable from {}: {error}", dir.display());
            Vec::new()
        }
    });
    *slot = Some((dir.clone(), Arc::clone(&fonts)));
    fonts
}

static EMPTY: std::sync::LazyLock<Arc<Vec<RlFont>>> =
    std::sync::LazyLock::new(|| Arc::new(Vec::new()));

/// empty without the game. first call pays for the extract
pub fn loaded() -> Arc<Vec<RlFont>> {
    cached()
}

/// no hyphens, directwrite reads "-bold" and "-thin" as style words and folds
/// those faces into one family
fn dwrite_family(id: &str) -> Option<&'static str> {
    Some(match id {
        "rl-digits" => "rldigits",
        "rl-header" => "rlheader",
        "rl-header-thin" => "rlheaderthin",
        "rl-body" => "rlbody",
        "rl-body-bold" => "rlbodybold",
        _ => return None,
    })
}

/// runs per drawn string, keep it allocation free
pub fn face_for(id: &str) -> Option<&'static str> {
    let family = dwrite_family(id)?;
    loaded().iter().any(|f| f.id == id).then_some(family)
}

// package

struct Cursor<'a> {
    b: &'a [u8],
    o: usize,
}

impl<'a> Cursor<'a> {
    fn new(b: &'a [u8]) -> Self {
        Self { b, o: 0 }
    }
    fn u32(&mut self) -> Result<u32, String> {
        let v = read_u32(self.b, self.o)?;
        self.o += 4;
        Ok(v)
    }
    fn i32(&mut self) -> Result<i32, String> {
        Ok(self.u32()? as i32)
    }
    fn skip(&mut self, n: usize) -> Result<(), String> {
        self.o = self.o.checked_add(n).ok_or("upk cursor overflow")?;
        if self.o > self.b.len() {
            return Err("upk cursor past end".into());
        }
        Ok(())
    }
    fn skip_fstring(&mut self) -> Result<(), String> {
        let n = self.i32()?;
        self.skip(if n < 0 { (-n as usize) * 2 } else { n as usize })
    }
    fn skip_array(&mut self, each: usize) -> Result<(), String> {
        let n = self.i32()?;
        if n < 0 {
            return Err("negative upk array".into());
        }
        self.skip(n as usize * each)
    }
}

fn read_u32(b: &[u8], o: usize) -> Result<u32, String> {
    b.get(o..o + 4)
        .map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
        .ok_or_else(|| "read past end of upk".to_string())
}

fn read_i32(b: &[u8], o: usize) -> Result<i32, String> {
    read_u32(b, o).map(|v| v as i32)
}

fn read_i64(b: &[u8], o: usize) -> Result<i64, String> {
    b.get(o..o + 8)
        .map(|s| i64::from_le_bytes([s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7]]))
        .ok_or_else(|| "read past end of upk".to_string())
}

/// decrypts the header and expands every chunk so package offsets line up
fn load_package(raw: &[u8]) -> Result<Vec<u8>, String> {
    let mut c = Cursor::new(raw);
    if c.u32()? != upk::UPK_MAGIC {
        return Err("not a upk package".into());
    }
    let licensee = (read_u32(raw, 4)? >> 16) as u16;
    c.skip(4)?; // version + licensee, already read
    c.o = 8;
    let total_header = c.i32()? as usize;
    c.skip_fstring()?;
    c.u32()?;
    let _name_count = c.i32()?;
    let name_offset = c.i32()? as usize;
    c.i32()?;
    c.i32()?;
    c.i32()?;
    c.i32()?;
    c.i32()?; // depends
    for _ in 0..4 {
        c.i32()?;
    }
    c.skip(16)?;
    c.skip_array(12)?;
    c.u32()?;
    c.u32()?;
    c.u32()?;
    c.skip_array(16)?;
    c.i32()?;
    let strings = c.i32()?;
    for _ in 0..strings.max(0) {
        c.skip_fstring()?;
    }
    let entries = c.i32()?;
    for _ in 0..entries.max(0) {
        c.skip(20)?;
        c.skip_array(4)?;
    }
    let gap = c.i32()? as usize;
    let chunk_info = c.i32()? as usize;

    let span = total_header
        .checked_sub(gap)
        .and_then(|v| v.checked_sub(name_offset))
        .ok_or("bad encrypted header bounds")?;
    let size = (span + 15) & !15;
    let enc = raw
        .get(name_offset..name_offset + size)
        .ok_or("upk too small for its header")?;
    let header = aes_decrypt(enc, &DEFAULT_UPK_KEY);

    let wide = licensee >= 22;
    let osz = if wide { 8 } else { 4 };
    let mut stride = osz + 4 + osz + 4;
    let count = read_i32(&header, chunk_info)?;
    if count <= 0 || count >= 1000 {
        return Err(format!("implausible chunk count {count}"));
    }
    let first = chunk_info + 4;
    let off_at = |buf: &[u8], at: usize| -> Result<usize, String> {
        Ok(if wide {
            read_i64(buf, at)? as usize
        } else {
            read_i32(buf, at)? as usize
        })
    };
    // some packages pad each row with 12 reserved bytes
    if count >= 2 {
        let expect = off_at(&header, first)? + read_i32(&header, first + osz)? as usize;
        if off_at(&header, first + stride + 12).ok() == Some(expect) {
            stride += 12;
        }
    }

    let mut logical = raw[..total_header.min(raw.len())].to_vec();
    logical[name_offset..name_offset + header.len()].copy_from_slice(&header);
    for i in 0..count as usize {
        let e = first + i * stride;
        let unc_off = off_at(&header, e)?;
        let cmp_off = off_at(&header, e + osz + 4)?;
        let (data, _, _) = upk::decomp_chunk_at(raw, cmp_off)
            .map_err(|_| format!("chunk {i} failed to decompress"))?;
        if logical.len() < unc_off + data.len() {
            logical.resize(unc_off + data.len(), 0);
        }
        logical[unc_off..unc_off + data.len()].copy_from_slice(&data);
    }
    Ok(logical)
}

fn aes_decrypt(data: &[u8], key: &[u8; 32]) -> Vec<u8> {
    let cipher = Aes256::new(GenericArray::from_slice(key));
    let mut out = data.to_vec();
    for block in out.chunks_exact_mut(16) {
        cipher.decrypt_block(GenericArray::from_mut_slice(block));
    }
    out
}

/// scaleform movies inside the package, located by their GFX/FWS header
fn find_movies(d: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while i + 8 < d.len() {
        let tag = &d[i..i + 3];
        if tag == b"GFX" || tag == b"FWS" {
            let ver = d[i + 3];
            if let Ok(len) = read_u32(d, i + 4) {
                let len = len as usize;
                if (1..=20).contains(&ver) && len > 1000 && i + len <= d.len() {
                    out.push(&d[i..i + len]);
                    i += len;
                    continue;
                }
            }
        }
        i += 1;
    }
    out
}

// swf

struct Bits<'a> {
    b: &'a [u8],
    byte: usize,
    bit: u32,
}

impl<'a> Bits<'a> {
    fn new(b: &'a [u8], at: usize) -> Self {
        Self { b, byte: at, bit: 0 }
    }
    fn ub(&mut self, n: u32) -> u32 {
        let mut v = 0u32;
        for _ in 0..n {
            let byte = self.b.get(self.byte).copied().unwrap_or(0);
            v = (v << 1) | ((byte >> (7 - self.bit)) & 1) as u32;
            self.bit += 1;
            if self.bit == 8 {
                self.bit = 0;
                self.byte += 1;
            }
        }
        v
    }
    fn sb(&mut self, n: u32) -> i32 {
        if n == 0 {
            return 0;
        }
        let v = self.ub(n);
        if v & (1 << (n - 1)) != 0 {
            v as i32 - (1i32 << n)
        } else {
            v as i32
        }
    }
}

enum Seg {
    Move(i32, i32),
    Line(i32, i32),
    Curve(i32, i32, i32, i32),
}

struct Font3 {
    name: String,
    codes: Vec<u16>,
    glyphs: Vec<Vec<Vec<Seg>>>,
    advances: Vec<i32>,
    ascent: i32,
    descent: i32,
    leading: i32,
}

/// walks the tag stream and reads every DefineFont3 (tag 75)
fn read_fonts(d: &[u8]) -> Vec<Font3> {
    let mut out = Vec::new();
    // header: magic(3) ver(1) len(4) then a RECT, framerate, frame count
    let Some(&first) = d.get(8) else {
        return out;
    };
    let nbits = (first >> 3) as usize;
    let mut p = 8 + (5 + nbits * 4).div_ceil(8) + 4;
    while p + 2 <= d.len() {
        let Ok(cl) = read_u16(d, p) else { break };
        let code = cl >> 6;
        let mut len = (cl & 0x3F) as usize;
        p += 2;
        if len == 0x3F {
            let Ok(l) = read_u32(d, p) else { break };
            len = l as usize;
            p += 4;
        }
        if p + len > d.len() {
            break;
        }
        if code == 75 {
            if let Some(f) = read_font3(&d[p..p + len]) {
                out.push(f);
            }
        }
        p += len;
        if code == 0 {
            break;
        }
    }
    out
}

fn read_u16(b: &[u8], o: usize) -> Result<u16, String> {
    b.get(o..o + 2)
        .map(|s| u16::from_le_bytes([s[0], s[1]]))
        .ok_or_else(|| "read past end".to_string())
}

fn read_font3(d: &[u8]) -> Option<Font3> {
    let flags = *d.get(2)?;
    let nlen = *d.get(4)? as usize;
    let name = String::from_utf8_lossy(d.get(5..5 + nlen)?)
        .trim_end_matches('\0')
        .to_string();
    let mut o = 5 + nlen;
    let nglyphs = read_u16(d, o).ok()? as usize;
    o += 2;
    let wide = flags & 0x08 != 0;
    let has_layout = flags & 0x80 != 0;

    let table = o;
    let mut offsets = Vec::with_capacity(nglyphs);
    for i in 0..nglyphs {
        offsets.push(if wide {
            read_u32(d, o + i * 4).ok()? as usize
        } else {
            read_u16(d, o + i * 2).ok()? as usize
        });
    }
    let code_off = if wide {
        read_u32(d, o + nglyphs * 4).ok()? as usize
    } else {
        read_u16(d, o + nglyphs * 2).ok()? as usize
    };

    let glyphs: Vec<_> = offsets
        .iter()
        .map(|&off| read_shape(d, table + off))
        .collect();

    let co = table + code_off;
    let mut codes = Vec::with_capacity(nglyphs);
    for i in 0..nglyphs {
        codes.push(read_u16(d, co + i * 2).ok()?);
    }

    let (mut ascent, mut descent, mut leading) = (0, 0, 0);
    let mut advances = vec![0; nglyphs];
    if has_layout {
        let lo = co + nglyphs * 2;
        ascent = read_u16(d, lo).ok()? as i16 as i32;
        descent = read_u16(d, lo + 2).ok()? as i16 as i32;
        leading = read_u16(d, lo + 4).ok()? as i16 as i32;
        for i in 0..nglyphs {
            advances[i] = read_u16(d, lo + 6 + i * 2).ok()? as i16 as i32;
        }
    }
    Some(Font3 {
        name,
        codes,
        glyphs,
        advances,
        ascent,
        descent,
        leading,
    })
}

fn read_shape(d: &[u8], at: usize) -> Vec<Vec<Seg>> {
    let mut r = Bits::new(d, at);
    let nfill = r.ub(4);
    let nline = r.ub(4);
    let mut contours = Vec::new();
    let mut cur: Vec<Seg> = Vec::new();
    let (mut x, mut y) = (0i32, 0i32);
    loop {
        if r.byte >= d.len() {
            break;
        }
        if r.ub(1) == 0 {
            let flags = r.ub(5);
            if flags == 0 {
                break;
            }
            if flags & 0x01 != 0 {
                let mb = r.ub(5);
                x = r.sb(mb);
                y = r.sb(mb);
                if !cur.is_empty() {
                    contours.push(std::mem::take(&mut cur));
                }
                cur.push(Seg::Move(x, y));
            }
            if flags & 0x02 != 0 {
                r.ub(nfill);
            }
            if flags & 0x04 != 0 {
                r.ub(nfill);
            }
            if flags & 0x08 != 0 {
                r.ub(nline);
            }
            if flags & 0x10 != 0 {
                break; // new styles never appear inside a glyph
            }
        } else if r.ub(1) == 1 {
            let nb = r.ub(4) + 2;
            let (dx, dy) = if r.ub(1) == 1 {
                (r.sb(nb), r.sb(nb))
            } else if r.ub(1) == 1 {
                (0, r.sb(nb))
            } else {
                (r.sb(nb), 0)
            };
            x += dx;
            y += dy;
            cur.push(Seg::Line(x, y));
        } else {
            let nb = r.ub(4) + 2;
            let cx = x + r.sb(nb);
            let cy = y + r.sb(nb);
            let ax = cx + r.sb(nb);
            let ay = cy + r.sb(nb);
            x = ax;
            y = ay;
            cur.push(Seg::Curve(cx, cy, ax, ay));
        }
    }
    if !cur.is_empty() {
        contours.push(cur);
    }
    contours
}

// sfnt

fn be16(v: i32) -> [u8; 2] {
    (v as i16).to_be_bytes()
}
fn beu16(v: u32) -> [u8; 2] {
    (v as u16).to_be_bytes()
}
fn beu32(v: u32) -> [u8; 4] {
    v.to_be_bytes()
}

/// scaleform y grows downward, truetype upward, so y flips on the way out
fn points(contours: &[Vec<Seg>]) -> Vec<Vec<(i32, i32, bool)>> {
    let s = SCALE;
    let mut out = Vec::new();
    for c in contours {
        let mut pts: Vec<(i32, i32, bool)> = Vec::new();
        for seg in c {
            match *seg {
                Seg::Move(x, y) | Seg::Line(x, y) => {
                    pts.push((x.div_euclid(s), (-y).div_euclid(s), true))
                }
                Seg::Curve(cx, cy, ax, ay) => {
                    pts.push((cx.div_euclid(s), (-cy).div_euclid(s), false));
                    pts.push((ax.div_euclid(s), (-ay).div_euclid(s), true));
                }
            }
        }
        // truetype closes contours itself
        if pts.len() > 1 {
            let (fx, fy, _) = pts[0];
            let (lx, ly, on) = *pts.last().unwrap();
            if on && fx == lx && fy == ly {
                pts.pop();
            }
        }
        if !pts.is_empty() {
            out.push(pts);
        }
    }
    out
}

fn glyf_entry(contours: &[Vec<Seg>]) -> (Vec<u8>, [i32; 4]) {
    let cs = points(contours);
    if cs.is_empty() {
        return (Vec::new(), [0, 0, 0, 0]);
    }
    let all: Vec<_> = cs.iter().flatten().copied().collect();
    let bbox = [
        all.iter().map(|p| p.0).min().unwrap(),
        all.iter().map(|p| p.1).min().unwrap(),
        all.iter().map(|p| p.0).max().unwrap(),
        all.iter().map(|p| p.1).max().unwrap(),
    ];
    let mut body = Vec::new();
    body.extend_from_slice(&be16(cs.len() as i32));
    for v in bbox {
        body.extend_from_slice(&be16(v));
    }
    let mut n = 0;
    for c in &cs {
        n += c.len();
        body.extend_from_slice(&beu16(n as u32 - 1));
    }
    body.extend_from_slice(&beu16(0)); // no hinting

    let (mut flags, mut xs, mut ys) = (Vec::new(), Vec::new(), Vec::new());
    let (mut px, mut py) = (0i32, 0i32);
    for (x, y, on) in all {
        let dx = x - px;
        let dy = y - py;
        px = x;
        py = y;
        let mut f: u8 = if on { 0x01 } else { 0x00 };
        if dx == 0 {
            f |= 0x10;
        } else if (-255..=255).contains(&dx) {
            f |= 0x02;
            if dx > 0 {
                f |= 0x10;
            }
            xs.push(dx.unsigned_abs() as u8);
        } else {
            xs.extend_from_slice(&be16(dx));
        }
        if dy == 0 {
            f |= 0x20;
        } else if (-255..=255).contains(&dy) {
            f |= 0x04;
            if dy > 0 {
                f |= 0x20;
            }
            ys.push(dy.unsigned_abs() as u8);
        } else {
            ys.extend_from_slice(&be16(dy));
        }
        flags.push(f);
    }
    body.extend_from_slice(&flags);
    body.extend_from_slice(&xs);
    body.extend_from_slice(&ys);
    while body.len() % 4 != 0 {
        body.push(0);
    }
    (body, bbox)
}

fn cmap4(map: &[(u16, u16)]) -> Vec<u8> {
    let mut segs: Vec<(u16, u16, u16)> = Vec::new(); // start, end, delta
    for &(code, gid) in map {
        if let Some(last) = segs.last_mut() {
            if code == last.1 + 1 && gid == last.1.wrapping_add(last.2).wrapping_add(1) {
                last.1 = code;
                continue;
            }
        }
        segs.push((code, code, gid.wrapping_sub(code)));
    }
    segs.push((0xFFFF, 0xFFFF, 1));

    let n = segs.len() as u32;
    let bits = 32 - n.leading_zeros() - 1;
    let search = 2u32.pow(bits) * 2;
    let mut sub = Vec::new();
    sub.extend_from_slice(&beu16(4));
    sub.extend_from_slice(&beu16(16 + n * 8));
    sub.extend_from_slice(&beu16(0));
    sub.extend_from_slice(&beu16(n * 2));
    sub.extend_from_slice(&beu16(search));
    sub.extend_from_slice(&beu16(bits));
    sub.extend_from_slice(&beu16(n * 2 - search));
    for s in &segs {
        sub.extend_from_slice(&beu16(s.1 as u32));
    }
    sub.extend_from_slice(&beu16(0));
    for s in &segs {
        sub.extend_from_slice(&beu16(s.0 as u32));
    }
    for s in &segs {
        sub.extend_from_slice(&beu16(s.2 as u32));
    }
    for _ in &segs {
        sub.extend_from_slice(&beu16(0));
    }

    let mut t = Vec::new();
    t.extend_from_slice(&beu16(0));
    t.extend_from_slice(&beu16(1));
    t.extend_from_slice(&beu16(3));
    t.extend_from_slice(&beu16(1));
    t.extend_from_slice(&beu32(12));
    t.extend_from_slice(&sub);
    t
}

fn name_table(family: &str) -> Vec<u8> {
    let vals = [
        (1u32, family.to_string()),
        (2, "Regular".into()),
        (3, format!("{family} (Rocket League)")),
        (4, family.to_string()),
        (5, "Version 1.0".into()),
        (6, family.replace(' ', "")),
    ];
    let mut recs = Vec::new();
    let mut strs: Vec<u8> = Vec::new();
    for (id, v) in &vals {
        let enc: Vec<u8> = v.encode_utf16().flat_map(u16::to_be_bytes).collect();
        recs.extend_from_slice(&beu16(3));
        recs.extend_from_slice(&beu16(1));
        recs.extend_from_slice(&beu16(0x409));
        recs.extend_from_slice(&beu16(*id));
        recs.extend_from_slice(&beu16(enc.len() as u32));
        recs.extend_from_slice(&beu16(strs.len() as u32));
        strs.extend_from_slice(&enc);
    }
    let mut t = Vec::new();
    t.extend_from_slice(&beu16(0));
    t.extend_from_slice(&beu16(vals.len() as u32));
    t.extend_from_slice(&beu16(6 + vals.len() as u32 * 12));
    t.extend_from_slice(&recs);
    t.extend_from_slice(&strs);
    t
}

fn checksum(d: &[u8]) -> u32 {
    let mut sum = 0u32;
    let mut i = 0;
    while i < d.len() {
        let mut w = [0u8; 4];
        for (j, slot) in w.iter_mut().enumerate() {
            *slot = d.get(i + j).copied().unwrap_or(0);
        }
        sum = sum.wrapping_add(u32::from_be_bytes(w));
        i += 4;
    }
    sum
}

fn build_ttf(f: &Font3, family: &str) -> Vec<u8> {
    let n = f.glyphs.len();
    let mut glyf = Vec::new();
    let mut loca = vec![0u32];
    let mut advances = vec![0i32]; // .notdef
    let mut bbox = [i32::MAX, i32::MAX, i32::MIN, i32::MIN];
    loca.push(0); // .notdef is empty
    for i in 0..n {
        let (body, bb) = glyf_entry(&f.glyphs[i]);
        if !body.is_empty() {
            bbox = [
                bbox[0].min(bb[0]),
                bbox[1].min(bb[1]),
                bbox[2].max(bb[2]),
                bbox[3].max(bb[3]),
            ];
        }
        glyf.extend_from_slice(&body);
        loca.push(glyf.len() as u32);
        advances.push(f.advances.get(i).copied().unwrap_or(0).div_euclid(SCALE).max(0));
    }
    if bbox[0] > bbox[2] {
        bbox = [0, 0, 0, 0];
    }
    let nglyphs = n as u32 + 1;
    let asc = f.ascent.div_euclid(SCALE);
    let desc = f.descent.div_euclid(SCALE);
    let lead = f.leading.div_euclid(SCALE).max(0);

    let mut map: Vec<(u16, u16)> = f
        .codes
        .iter()
        .enumerate()
        .map(|(i, &c)| (c, i as u16 + 1))
        .collect();
    map.sort_unstable();
    map.dedup_by_key(|e| e.0);

    let long_loca = *loca.last().unwrap() > 0x1FFFF;
    let mut head = Vec::new();
    head.extend_from_slice(&beu32(0x0001_0000));
    head.extend_from_slice(&beu32(0x0001_0000));
    head.extend_from_slice(&beu32(0)); // checksum adjustment, filled in later
    head.extend_from_slice(&beu32(0x5F0F_3CF5));
    head.extend_from_slice(&beu16(0x000B));
    head.extend_from_slice(&beu16(UPEM as u32));
    head.extend_from_slice(&[0u8; 16]); // created + modified
    for v in bbox {
        head.extend_from_slice(&be16(v));
    }
    head.extend_from_slice(&beu16(0));
    head.extend_from_slice(&beu16(8));
    head.extend_from_slice(&be16(2));
    head.extend_from_slice(&be16(if long_loca { 1 } else { 0 }));
    head.extend_from_slice(&be16(0));

    let mut hhea = Vec::new();
    hhea.extend_from_slice(&beu32(0x0001_0000));
    hhea.extend_from_slice(&be16(asc));
    hhea.extend_from_slice(&be16(-desc));
    hhea.extend_from_slice(&be16(lead));
    hhea.extend_from_slice(&beu16(advances.iter().copied().max().unwrap_or(0) as u32));
    hhea.extend_from_slice(&be16(bbox[0]));
    hhea.extend_from_slice(&be16(bbox[1]));
    hhea.extend_from_slice(&be16(bbox[2]));
    hhea.extend_from_slice(&be16(1));
    for _ in 0..7 {
        hhea.extend_from_slice(&be16(0));
    }
    hhea.extend_from_slice(&beu16(nglyphs));

    let mut maxp = Vec::new();
    maxp.extend_from_slice(&beu32(0x0001_0000));
    maxp.extend_from_slice(&beu16(nglyphs));
    for v in [255u32, 64, 0, 0, 2, 0, 0, 0, 0, 0, 0, 0, 0] {
        maxp.extend_from_slice(&beu16(v));
    }

    let mut hmtx = Vec::new();
    for a in &advances {
        hmtx.extend_from_slice(&beu16(*a as u32));
        hmtx.extend_from_slice(&be16(0));
    }

    let loca_b: Vec<u8> = if long_loca {
        loca.iter().flat_map(|v| beu32(*v)).collect()
    } else {
        loca.iter().flat_map(|v| beu16(v / 2)).collect()
    };

    let mut os2 = Vec::new();
    os2.extend_from_slice(&beu16(4));
    os2.extend_from_slice(&be16(advances.iter().copied().max().unwrap_or(1000) / 2));
    os2.extend_from_slice(&beu16(400));
    os2.extend_from_slice(&beu16(5));
    os2.extend_from_slice(&beu16(0));
    for _ in 0..10 {
        os2.extend_from_slice(&be16(0));
    }
    os2.extend_from_slice(&be16(0)); // family class
    os2.extend_from_slice(&[0u8; 10]); // panose
    for _ in 0..4 {
        os2.extend_from_slice(&beu32(0xFFFF_FFFF));
    }
    os2.extend_from_slice(b"HBNX");
    os2.extend_from_slice(&beu16(0x0040));
    os2.extend_from_slice(&beu16(map.first().map(|e| e.0 as u32).unwrap_or(32)));
    os2.extend_from_slice(&beu16(
        map.iter().map(|e| e.0 as u32).max().unwrap_or(126).min(0xFFFF),
    ));
    os2.extend_from_slice(&be16(asc));
    os2.extend_from_slice(&be16(-desc));
    os2.extend_from_slice(&be16(lead));
    os2.extend_from_slice(&beu16(asc.max(0) as u32));
    os2.extend_from_slice(&beu16(desc.max(0) as u32));
    os2.extend_from_slice(&beu32(1));
    os2.extend_from_slice(&beu32(0));
    os2.extend_from_slice(&be16(asc / 2));
    os2.extend_from_slice(&be16(asc));
    os2.extend_from_slice(&beu16(0));
    os2.extend_from_slice(&beu16(32));
    os2.extend_from_slice(&beu16(0));

    let mut post = Vec::new();
    post.extend_from_slice(&beu32(0x0003_0000));
    for _ in 0..8 {
        post.extend_from_slice(&beu32(0));
    }

    let tables: Vec<(&[u8; 4], Vec<u8>)> = vec![
        (b"OS/2", os2),
        (b"cmap", cmap4(&map)),
        (b"glyf", glyf),
        (b"head", head),
        (b"hhea", hhea),
        (b"hmtx", hmtx),
        (b"loca", loca_b),
        (b"maxp", maxp),
        (b"name", name_table(family)),
        (b"post", post),
    ];

    let count = tables.len() as u32;
    let bits = 32 - count.leading_zeros() - 1;
    let search = 2u32.pow(bits) * 16;
    let mut out = Vec::new();
    out.extend_from_slice(&beu32(0x0001_0000));
    out.extend_from_slice(&beu16(count));
    out.extend_from_slice(&beu16(search));
    out.extend_from_slice(&beu16(bits));
    out.extend_from_slice(&beu16(count * 16 - search));

    let mut offset = 12 + count as usize * 16;
    let mut dir = Vec::new();
    let mut body = Vec::new();
    let mut head_at = 0usize;
    for (tag, data) in &tables {
        if *tag == b"head" {
            head_at = offset;
        }
        dir.extend_from_slice(*tag);
        dir.extend_from_slice(&beu32(checksum(data)));
        dir.extend_from_slice(&beu32(offset as u32));
        dir.extend_from_slice(&beu32(data.len() as u32));
        body.extend_from_slice(data);
        let pad = (4 - data.len() % 4) % 4;
        body.extend(std::iter::repeat_n(0u8, pad));
        offset += data.len() + pad;
    }
    out.extend_from_slice(&dir);
    out.extend_from_slice(&body);

    let adj = 0xB1B0_AFBAu32.wrapping_sub(checksum(&out));
    out[head_at + 8..head_at + 12].copy_from_slice(&beu32(adj));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg_square() -> Vec<Vec<Seg>> {
        vec![vec![
            Seg::Move(0, 0),
            Seg::Line(2048 * SCALE, 0),
            Seg::Line(2048 * SCALE, 2048 * SCALE),
            Seg::Line(0, 2048 * SCALE),
        ]]
    }

    #[test]
    fn shape_points_flip_y_and_scale_to_the_em() {
        let pts = points(&seg_square());
        assert_eq!(pts.len(), 1, "one contour");
        // y flips, x keeps sign, both land on the 2048 em
        assert_eq!(pts[0][0], (0, 0, true));
        assert_eq!(pts[0][1], (2048, 0, true));
        assert_eq!(pts[0][2], (2048, -2048, true));
    }

    #[test]
    fn a_curve_emits_an_off_curve_control_then_an_anchor() {
        let contours = vec![vec![
            Seg::Move(0, 0),
            Seg::Curve(100 * SCALE, 0, 200 * SCALE, 0),
        ]];
        let pts = points(&contours);
        assert_eq!(pts[0][1], (100, 0, false), "control point is off curve");
        assert_eq!(pts[0][2], (200, 0, true), "anchor is on curve");
    }

    #[test]
    fn builds_an_sfnt_with_the_expected_tables() {
        let font = Font3 {
            name: "Test".into(),
            codes: vec![b'A' as u16],
            glyphs: vec![seg_square()],
            advances: vec![2048 * SCALE],
            ascent: 1600 * SCALE,
            descent: 400 * SCALE,
            leading: 0,
        };
        let ttf = build_ttf(&font, "rl-test");
        assert_eq!(&ttf[0..4], &[0x00, 0x01, 0x00, 0x00], "sfnt version");
        let count = u16::from_be_bytes([ttf[4], ttf[5]]) as usize;
        assert_eq!(count, 10, "ten tables");
        let tags: Vec<&[u8]> = (0..count).map(|i| &ttf[12 + i * 16..12 + i * 16 + 4]).collect();
        for want in [&b"glyf"[..], b"head", b"cmap", b"hmtx", b"loca", b"maxp"] {
            assert!(tags.contains(&want), "missing {:?}", std::str::from_utf8(want));
        }
    }

    // needs the game: cargo test -p hebnix-app -- --ignored extracts_from_a_real_install
    #[test]
    #[ignore]
    fn extracts_from_a_real_install() {
        let rl = std::env::var("HEBNIX_RL_DIR")
            .unwrap_or_else(|_| r"E:\SteamLibrary\steamapps\common\rocketleague".into());
        let fonts = extract(Path::new(&rl)).expect("extract rl fonts");
        let dump = std::env::temp_dir().join("hebnix_rl_fonts");
        let _ = std::fs::create_dir_all(&dump);
        for f in &fonts {
            assert_eq!(&f.ttf[0..4], &[0x00, 0x01, 0x00, 0x00], "{} sfnt", f.id);
            let _ = std::fs::write(dump.join(format!("{}.ttf", f.id)), &f.ttf);
        }
        let ids: Vec<&str> = fonts.iter().map(|f| f.id).collect();
        let faces: Vec<&str> = fonts.iter().map(|f| f.face.as_str()).collect();
        println!("dumped to {} -> {ids:?} from {faces:?}", dump.display());
        for want in ["rl-header", "rl-digits", "rl-body", "rl-body-bold", "rl-header-thin"] {
            assert!(ids.contains(&want), "missing {want} in {ids:?}");
        }
    }

    #[test]
    fn glyph_zero_is_notdef_so_real_glyphs_start_at_one() {
        let font = Font3 {
            name: "T".into(),
            codes: vec![b'Z' as u16],
            glyphs: vec![seg_square()],
            advances: vec![100],
            ascent: 0,
            descent: 0,
            leading: 0,
        };
        let ttf = build_ttf(&font, "rl-test");
        let count = u16::from_be_bytes([ttf[4], ttf[5]]) as usize;
        let maxp = (0..count)
            .map(|i| &ttf[12 + i * 16..12 + i * 16 + 16])
            .find(|e| &e[0..4] == b"maxp")
            .expect("maxp");
        let off = u32::from_be_bytes([maxp[8], maxp[9], maxp[10], maxp[11]]) as usize;
        let nglyphs = u16::from_be_bytes([ttf[off + 4], ttf[off + 5]]);
        assert_eq!(nglyphs, 2, "notdef plus the one real glyph");
    }
}
