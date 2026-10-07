//! Experimental ball geometry swaps. The original collision vertices and kDOP
//! are retained; donor vertices are appended and only draw indices select them.
use super::upk_package::{ExportEntry, UpkPackage, strip};
use std::{ops::Range, path::Path};
#[derive(Clone, Debug)]
struct Bulk {
    stride: usize,
    count: usize,
    data: Range<usize>,
}
struct Reader<'a> {
    b: &'a [u8],
    at: usize,
}
impl Reader<'_> {
    fn take(&mut self, n: usize) -> Result<Range<usize>, String> {
        let end = self.at.checked_add(n).ok_or("Mesh size overflow")?;
        if end > self.b.len() {
            return Err(format!("Truncated mesh at {} + {n}", self.at));
        }
        let r = self.at..end;
        self.at = end;
        Ok(r)
    }
    fn int(&mut self) -> Result<i32, String> {
        let r = self.take(4)?;
        Ok(i32::from_le_bytes(self.b[r].try_into().unwrap()))
    }
    fn count(&mut self) -> Result<usize, String> {
        let n = self.int()?;
        if !(0..=2_000_000).contains(&n) {
            return Err(format!("Invalid mesh count {n} at {}", self.at - 4));
        }
        Ok(n as usize)
    }
    fn bulk(&mut self) -> Result<Bulk, String> {
        let stride = self.count()?;
        let count = self.count()?;
        if stride > 1024 {
            return Err("Invalid buffer stride".into());
        }
        let data = self.take(stride.checked_mul(count).ok_or("Buffer overflow")?)?;
        Ok(Bulk {
            stride,
            count,
            data,
        })
    }
}
#[derive(Debug)]
struct Lod {
    sections: Vec<Vec<u8>>,
    positions: Bulk,
    uv_header: [i32; 4],
    uv: Bulk,
    colors: Option<Bulk>,
    indices: Bulk,
    wire: Bulk,
    adjacency: Bulk,
}
#[derive(Debug)]
struct Mesh {
    native: usize,
    lods_start: usize,
    lods: Vec<Lod>,
    tail: usize,
}
fn parse(p: &UpkPackage, e: &ExportEntry) -> Result<Mesh, String> {
    if p.image.get(4..6) != Some(&868u16.to_le_bytes()) {
        return Err("Unsupported mesh package version".into());
    }
    let (_, native) = p.serialized_props(e)?;
    let b = &p.image[e.serial_offset..e.serial_offset + e.serial_size];
    let mut r = Reader { b, at: native };
    r.take(56)?; // Bounds, native BodySetup reference, kDOP bounds
    let nodes = r.bulk()?;
    let triangles = r.bulk()?;
    if nodes.stride != 6 || triangles.stride != 8 {
        return Err("Unsupported collision tree format".into());
    }
    if r.int()? != 18 {
        return Err("Unsupported native mesh version".into());
    }
    for _ in 0..4 {
        if r.int()? != 0 {
            return Err("Unsupported auxiliary mesh data".into());
        }
    }
    let lods_start = r.at;
    let count = r.count()?;
    if count == 0 || count > 8 {
        return Err("Invalid mesh LOD count".into());
    }
    let mut lods = Vec::new();
    for _ in 0..count {
        // Rocket League stripped source bulk data has flags/count/size only.
        if r.int()? != 65536 || r.int()? != 0 || r.int()? != 0 {
            return Err("Mesh has unstripped source bulk data".into());
        }
        let section_count = r.count()?;
        if section_count == 0 || section_count > 32 {
            return Err("Invalid mesh sections".into());
        }
        let mut sections = Vec::new();
        for _ in 0..section_count {
            let start = r.at;
            r.take(36)?;
            let fragments = r.count()?;
            r.take(fragments * 8)?;
            let flag = r.take(1)?;
            if b[flag][0] != 0 {
                return Err("Unsupported platform mesh section".into());
            }
            sections.push(b[start..r.at].to_vec());
        }
        if r.int()? != 12 {
            return Err(format!("Unsupported positions stride at {}", r.at - 4));
        }
        let verts = r.count()?;
        let positions = r.bulk()?;
        if positions.stride != 12 || positions.count != verts {
            return Err("Position count mismatch".into());
        }
        let uv_header = [r.int()?, r.int()?, r.int()?, r.int()?];
        let uv = r.bulk()?;
        if uv.count != verts || uv.stride as i32 != uv_header[1] || uv_header[2] != verts as i32 {
            return Err("UV count mismatch".into());
        }
        let color_stride = r.count()?;
        let color_count = r.count()?;
        let colors = if color_count > 0 {
            let c = r.bulk()?;
            if color_stride != 4 || c.stride != 4 || c.count != verts || color_count != verts {
                return Err("Color count mismatch".into());
            }
            Some(c)
        } else {
            None
        };
        if r.count()? != verts {
            return Err("LOD vertex count mismatch".into());
        }
        let indices = r.bulk()?;
        let wire = r.bulk()?;
        let adjacency = r.bulk()?;
        for buffer in [&indices, &wire, &adjacency] {
            if buffer.stride != 2 {
                return Err("Only 16-bit mesh indices are supported".into());
            }
            for ix in b[buffer.data.clone()].chunks_exact(2) {
                if u16::from_le_bytes(ix.try_into().unwrap()) as usize >= verts {
                    return Err("Index outside vertex buffer".into());
                }
            }
        }
        lods.push(Lod {
            sections,
            positions,
            uv_header,
            uv,
            colors,
            indices,
            wire,
            adjacency,
        });
    }
    Ok(Mesh {
        native,
        lods_start,
        lods,
        tail: r.at,
    })
}
fn put(b: &mut Vec<u8>, n: usize) {
    b.extend_from_slice(&(n as u32).to_le_bytes())
}
fn bulk(b: &mut Vec<u8>, stride: usize, data: &[u8]) {
    put(b, stride);
    put(b, data.len() / stride);
    b.extend_from_slice(data)
}
fn get_i(b: &[u8], at: usize) -> i32 {
    i32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}
fn set_i(b: &mut [u8], at: usize, n: i32) {
    b[at..at + 4].copy_from_slice(&n.to_le_bytes())
}
fn mesh_export<'a>(p: &'a UpkPackage, name: &str) -> Result<&'a ExportEntry, String> {
    p.exports
        .iter()
        .find(|e| strip(&p.class_of(e)) == "StaticMesh" && strip(&p.name_of(e.object_name)) == name)
        .ok_or_else(|| format!("Missing static mesh {name}"))
}
pub fn catalog(path: &Path) -> Result<Vec<String>, String> {
    let p = UpkPackage::load(path)?;
    let mut result = Vec::new();
    for e in &p.exports {
        let name = p.name_of(e.object_name);
        let name = strip(&name);
        if strip(&p.class_of(e)) == "StaticMesh"
            && (name.starts_with("Ball_")
                || name.starts_with("ball_")
                || [
                    "HockeyPuck_SM",
                    "eggball_sm",
                    "Football_06_SM",
                    "PizzaCheezyPuck_SM",
                    "PumpkinCube2_SM",
                    "GodBall_RL_SM",
                ]
                .contains(&name))
        {
            match parse(&p, e) {
                Ok(m) => result.push(format!(
                    "{name}: {} LOD(s), {} vertices",
                    m.lods.len(),
                    m.lods[0].positions.count
                )),
                Err(err) => result.push(format!("{name}: unsupported ({err})")),
            }
        }
    }
    Ok(result)
}
/// Make a geometry-only trial using the existing normal-ball material.
/// No source game file is modified. All non-mesh exports must round-trip unchanged.
pub fn stage(
    source: &Path,
    donor_path: &Path,
    donor_name: &str,
    output: &Path,
) -> Result<String, String> {
    stage_for(source, donor_path, "Ball_DefaultBall00", donor_name, output)
}
pub fn stage_for(
    source: &Path,
    donor_path: &Path,
    target_name: &str,
    donor_name: &str,
    output: &Path,
) -> Result<String, String> {
    if target_name == donor_name {
        return Err("Choose a different visual ball".into());
    }
    if output.exists()
        && source.canonicalize().map_err(|e| e.to_string())?
            == output.canonicalize().map_err(|e| e.to_string())?
    {
        return Err("Stage into a separate file".into());
    }
    let mut target = UpkPackage::load(source)?;
    let donor = UpkPackage::load(donor_path)?;
    let te = mesh_export(&target, target_name)?.clone();
    let de = mesh_export(&donor, donor_name)?.clone();
    let tm = parse(&target, &te)?;
    let dm = parse(&donor, &de)?;
    let tb = target.image[te.serial_offset..te.serial_offset + te.serial_size].to_vec();
    let db = &donor.image[de.serial_offset..de.serial_offset + de.serial_size];
    let mut result = tb[..tm.lods_start].to_vec();
    put(&mut result, tm.lods.len());
    let mut scale = 1.0f32;
    // Fit visual geometry inside original render bounds; do not change bounds/scale.
    for axis in 0..3 {
        let origin = f32::from_le_bytes(
            tb[tm.native + axis * 4..tm.native + axis * 4 + 4]
                .try_into()
                .unwrap(),
        );
        let extent = f32::from_le_bytes(
            tb[tm.native + 12 + axis * 4..tm.native + 16 + axis * 4]
                .try_into()
                .unwrap(),
        );
        let available = extent - origin.abs();
        if !available.is_finite() || available <= 0.0 {
            return Err("Invalid target bounds".into());
        }
        for lod in &dm.lods {
            for vertex in db[lod.positions.data.clone()].chunks_exact(12) {
                let x =
                    f32::from_le_bytes(vertex[axis * 4..axis * 4 + 4].try_into().unwrap()).abs();
                if !x.is_finite() {
                    return Err("Invalid donor vertex".into());
                }
                if x > 0.0 {
                    scale = scale.min(available / x)
                }
            }
        }
    }
    for (i, old) in tm.lods.iter().enumerate() {
        let new = &dm.lods[i.min(dm.lods.len() - 1)];
        let base = old.positions.count;
        let count = base + new.positions.count;
        if count > u16::MAX as usize {
            return Err("Combined mesh exceeds 16-bit vertex limit".into());
        }
        put(&mut result, 65536);
        put(&mut result, 0);
        put(&mut result, 0);
        put(&mut result, old.sections.len() + new.sections.len());
        // Keep original section slots addressable by the untouched kDOP
        // triangles. Empty draw ranges hide them; donor sections are appended.
        for original in &old.sections {
            let mut s = original.clone();
            set_i(&mut s, 16, 0);
            set_i(&mut s, 20, 0);
            let fragments = get_i(&s, 36) as usize;
            for i in 0..fragments {
                set_i(&mut s, 40 + i * 8, 0);
                set_i(&mut s, 44 + i * 8, 0);
            }
            result.extend_from_slice(&s);
        }
        for section in &new.sections {
            let mut s = section.clone();
            // The donor material slot is already present in Mutators_Balls_SF.
            // Preserve it so a puck uses its own skin instead of the normal
            // ball material applied to the newly copied geometry.
            for at in [24, 28] {
                let n = get_i(&s, at);
                if n < 0 || n as usize >= new.positions.count {
                    return Err("Invalid section vertex range".into());
                }
                set_i(&mut s, at, n + base as i32);
            }
            result.extend_from_slice(&s);
        }
        let mut positions = tb[old.positions.data.clone()].to_vec();
        for v in db[new.positions.data.clone()].chunks_exact(4) {
            positions.extend_from_slice(
                &(f32::from_le_bytes(v.try_into().unwrap()) * scale).to_le_bytes(),
            );
        }
        put(&mut result, 12);
        put(&mut result, count);
        bulk(&mut result, 12, &positions);
        let mut uv = vec![0; new.uv.stride * base];
        uv.extend_from_slice(&db[new.uv.data.clone()]);
        for (field, n) in new.uv_header.iter().enumerate() {
            put(&mut result, if field == 2 { count } else { *n as usize })
        }
        bulk(&mut result, new.uv.stride, &uv);
        if let Some(c) = &new.colors {
            let mut colors = vec![255; 4 * base];
            colors.extend_from_slice(&db[c.data.clone()]);
            put(&mut result, 4);
            put(&mut result, count);
            bulk(&mut result, 4, &colors);
        } else {
            put(&mut result, 0);
            put(&mut result, 0)
        }
        put(&mut result, count);
        for b in [&new.indices, &new.wire, &new.adjacency] {
            let data: Vec<u8> = db[b.data.clone()]
                .chunks_exact(2)
                .flat_map(|v| {
                    (u16::from_le_bytes(v.try_into().unwrap()) + base as u16).to_le_bytes()
                })
                .collect();
            bulk(&mut result, 2, &data);
        }
    }
    result.extend_from_slice(&tb[tm.tail..]);
    let originals: Vec<_> = target
        .exports
        .iter()
        .map(|e| target.image[e.serial_offset..e.serial_offset + e.serial_size].to_vec())
        .collect();
    target.replace_export_payload(te.table_index, &result)?;
    target.save(output)?;
    let checked = UpkPackage::load(output)?;
    let changed = &checked.exports[te.table_index];
    let check = parse(&checked, changed)?;
    let cb = &checked.image[changed.serial_offset..changed.serial_offset + changed.serial_size];
    if cb[..check.lods_start] != tb[..tm.lods_start] {
        return Err("Collision prefix changed".into());
    }
    for (old, new) in tm.lods.iter().zip(&check.lods) {
        if cb[new.positions.data.start..new.positions.data.start + old.positions.count * 12]
            != tb[old.positions.data.clone()]
        {
            return Err("Original collision vertices changed".into());
        }
    }
    for (index, e) in checked.exports.iter().enumerate() {
        if index != te.table_index
            && checked.image[e.serial_offset..e.serial_offset + e.serial_size] != originals[index]
        {
            return Err(format!("Unrelated export changed: {index}"));
        }
    }
    Ok(format!(
        "{donor_name}: render geometry staged (scale {scale:.4}); actor, material, collision exports, kDOP and original collision vertices preserved"
    ))
}

pub const BALLS: &[(&str, &str)] = &[
    ("Normal", "Ball_DefaultBall00"),
    ("Puck", "HockeyPuck_SM"),
    ("Egg", "eggball_sm"),
    ("Cube", "Ball_CubeBall_SM"),
    ("Basketball", "Ball_Basketball_SM"),
    ("Beach ball", "Ball_Beachball_SM"),
    ("Football", "Football_06_SM"),
    ("Pizza puck", "PizzaCheezyPuck_SM"),
    ("Pumpkin cube", "PumpkinCube2_SM"),
    ("Dropshot", "Ball_Breakout_SM"),
    ("God ball", "GodBall_RL_SM"),
    ("Nike", "Ball_Nike_SM"),
    ("Adidas", "ball_adidas_sm"),
    ("Soccer", "Ball_Soccer_SM"),
    ("Luminous Airplane", "Ball_LuminousAirplane_SM"),
    ("Sphere Gulp", "Ball_SphereGulp_SM"),
];
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct SavedSwap {
    pub source: usize,
    pub donor: usize,
    files: Vec<SavedMesh>,
}
#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct SavedMesh {
    file: String,
    before: String,
    after: String,
    package_before: String,
    #[serde(default)]
    textures: Vec<SavedTexture>,
}
fn hash(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(bytes))
}
fn state_dir(backups: &Path) -> std::path::PathBuf {
    backups.join("ball_visual_swaps")
}
pub fn active(backups: &Path) -> Result<Vec<SavedSwap>, String> {
    let path = state_dir(backups).join("state.json");
    let data = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.to_string()),
    };
    let swaps: Vec<SavedSwap> =
        serde_json::from_slice(&data).map_err(|e| format!("Invalid ball visual state: {e}"))?;
    let mut seen = std::collections::HashSet::new();
    for s in &swaps {
        if s.source >= BALLS.len() || s.donor >= BALLS.len() || !seen.insert(s.source) {
            return Err("Invalid ball swap selection in state".into());
        }
        for f in &s.files {
            if Path::new(&f.file).file_name().and_then(|n| n.to_str()) != Some(&f.file)
                || f.file.contains(['/', '\\', ':'])
                || !f.file.to_ascii_lowercase().ends_with(".upk")
            {
                return Err("Invalid ball package in state".into());
            }
        }
    }
    Ok(swaps)
}
fn save_state(backups: &Path, state: &[SavedSwap]) -> Result<(), String> {
    let dir = state_dir(backups);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let temp = dir.join("state.json.tmp");
    std::fs::write(
        &temp,
        serde_json::to_vec_pretty(state).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    std::fs::rename(temp, dir.join("state.json")).map_err(|e| e.to_string())
}
fn payload(p: &UpkPackage, name: &str) -> Result<Vec<u8>, String> {
    let e = mesh_export(p, name)?;
    Ok(p.image[e.serial_offset..e.serial_offset + e.serial_size].to_vec())
}
fn mesh_backup(backups: &Path, source: usize, file: &str) -> std::path::PathBuf {
    state_dir(backups).join(format!("{source}-{file}.mesh"))
}
fn selection(source: usize, donor: usize) -> Result<(), String> {
    if source >= BALLS.len() || donor >= BALLS.len() {
        return Err("Invalid ball selection".into());
    }
    if source == donor {
        return Err("Choose two different balls".into());
    }
    Ok(())
}

pub fn apply(cooked: &Path, backups: &Path, source: usize, donor: usize) -> Result<(), String> {
    apply_with_colours(cooked, backups, source, donor, false)
}
pub fn apply_with_colours(
    cooked: &Path,
    backups: &Path,
    source: usize,
    donor: usize,
    colours: bool,
) -> Result<(), String> {
    selection(source, donor)?;
    super::backup_guard::prepare(cooked, backups, "ball-visual-build.sha256", |name| {
        name.eq_ignore_ascii_case("BallVisual")
    })?;
    let mut swaps = active(backups)?;
    if swaps.iter().any(|s| s.source == source) {
        return Err(
            "This ball already has a visual swap. Restore it before applying another.".into(),
        );
    }
    let dir = state_dir(backups);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let colour_image = if colours {
        Some(donor_colours(cooked, source, donor)?)
    } else {
        None
    };
    let work = dir.join(format!("stage-{}", rand::random::<u64>()));
    std::fs::create_dir(&work).map_err(|e| e.to_string())?;
    let result = (|| {
        // Build a pristine donor package even if this mesh was swapped earlier.
        let donor_path = work.join("donor.upk");
        let mut dp = UpkPackage::load(&cooked.join("Mutators_Balls_SF.upk"))?;
        if let Some(s) = swaps.iter().find(|s| s.source == donor) {
            if let Some(f) = s
                .files
                .iter()
                .find(|f| f.file.eq_ignore_ascii_case("Mutators_Balls_SF.upk"))
            {
                let old = std::fs::read(mesh_backup(backups, donor, &f.file))
                    .map_err(|e| e.to_string())?;
                if hash(&old) != f.before {
                    return Err("Donor backup checksum failed".into());
                }
                let e = mesh_export(&dp, BALLS[donor].1)?.table_index;
                dp.replace_export_payload(e, &old)?;
            }
        }
        dp.save(&donor_path)?;
        drop(dp);
        let mut entries: Vec<_> = std::fs::read_dir(cooked)
            .map_err(|e| e.to_string())?
            .filter_map(Result::ok)
            .filter(|e| {
                e.file_name()
                    .to_str()
                    .is_some_and(super::patch_core::gameinfo::is_ball_upk)
            })
            .collect();
        entries.sort_by_key(|e| e.file_name());
        let mut files = Vec::new();
        for entry in entries {
            let file = entry.file_name().to_string_lossy().to_string();
            let package_before = hash(&std::fs::read(entry.path()).map_err(|e| e.to_string())?);
            let p = UpkPackage::load(&entry.path()).map_err(|e| format!("{file}: {e}"))?;
            let Ok(original) = payload(&p, BALLS[source].1) else {
                continue;
            };
            drop(p);
            let staged = work.join(&file);
            stage_for(
                &entry.path(),
                &donor_path,
                BALLS[source].1,
                BALLS[donor].1,
                &staged,
            )
            .map_err(|e| format!("{file}: {e}"))?;
            let textures = if let Some(image) = &colour_image {
                merge_colours(&staged, image, backups, source, &file)?
            } else {
                Vec::new()
            };
            let after = payload(&UpkPackage::load(&staged)?, BALLS[source].1)?;
            std::fs::write(mesh_backup(backups, source, &file), &original)
                .map_err(|e| e.to_string())?;
            files.push(SavedMesh {
                file,
                before: hash(&original),
                after: hash(&after),
                package_before,
                textures,
            });
        }
        if files.is_empty() {
            return Err("No installed package contains the selected ball mesh".into());
        }
        // Record recovery information before installation. Restore accepts either
        // before or after payload, so a interrupted multi-file apply is recoverable.
        swaps.push(SavedSwap {
            source,
            donor,
            files: files.clone(),
        });
        save_state(backups, &swaps)?;
        for f in &files {
            let live = cooked.join(&f.file);
            if hash(&std::fs::read(&live).map_err(|e| e.to_string())?) != f.package_before {
                return Err(format!(
                    "{} changed during staging; Restore to recover",
                    f.file
                ));
            }
            let current = UpkPackage::load(&live)?;
            if hash(&payload(&current, BALLS[source].1)?) != f.before {
                return Err(format!(
                    "{} changed while preparing the swap; Restore to recover",
                    f.file
                ));
            }
            drop(current);
            std::fs::rename(work.join(&f.file), &live)
                .map_err(|e| format!("Install failed: {e}. Use Restore to recover."))?;
        }
        Ok(())
    })();
    // No recursive cleanup: only direct staging files generated above.
    if let Ok(entries) = std::fs::read_dir(&work) {
        for e in entries.flatten() {
            if e.file_type().is_ok_and(|t| t.is_file()) {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
    let _ = std::fs::remove_dir(work);
    result
}

pub fn restore(cooked: &Path, backups: &Path, source: usize) -> Result<(), String> {
    if source >= BALLS.len() {
        return Err("Invalid ball selection".into());
    }
    super::backup_guard::check(cooked, backups, "ball-visual-build.sha256", |name| {
        name.eq_ignore_ascii_case("BallVisual")
    })?;
    let mut swaps = active(backups)?;
    let swap = swaps
        .iter()
        .find(|s| s.source == source)
        .ok_or("This ball has no saved visual swap")?
        .clone();
    // Check every affected mesh before modifying any package.
    for f in &swap.files {
        let old =
            std::fs::read(mesh_backup(backups, source, &f.file)).map_err(|e| e.to_string())?;
        if hash(&old) != f.before {
            return Err("Ball backup checksum failed".into());
        }
        let p = UpkPackage::load(&cooked.join(&f.file))?;
        validate_textures(&p, backups, source, f)?;
        let current = hash(&payload(&p, BALLS[source].1)?);
        if current != f.before && current != f.after {
            return Err(format!(
                "{} ball mesh changed since this swap. Restore stopped to preserve newer changes.",
                f.file
            ));
        }
    }
    for f in &swap.files {
        let live = cooked.join(&f.file);
        let mut p = UpkPackage::load(&live)?;
        if hash(&payload(&p, BALLS[source].1)?) == f.before && f.textures.is_empty() {
            continue;
        }
        let original =
            std::fs::read(mesh_backup(backups, source, &f.file)).map_err(|e| e.to_string())?;
        let index = mesh_export(&p, BALLS[source].1)?.table_index;
        p.replace_export_payload(index, &original)?;
        validate_textures(&p, backups, source, f)?;
        for texture in &f.textures {
            let bytes = std::fs::read(texture_backup(backups, source, &f.file, texture.index))
                .map_err(|e| e.to_string())?;
            p.replace_export_payload(texture.index, &bytes)?;
        }
        let temp = state_dir(backups).join(format!("restore-{}", f.file));
        p.save(&temp)?;
        let check = UpkPackage::load(&temp)?;
        for texture in &f.textures {
            let e = &check.exports[texture.index];
            if hash(&check.image[e.serial_offset..e.serial_offset + e.serial_size])
                != texture.before
            {
                return Err("Restored colour texture failed validation".into());
            }
        }
        if hash(&payload(&check, BALLS[source].1)?) != f.before {
            return Err("Restored mesh failed validation".into());
        }
        std::fs::rename(&temp, &live).map_err(|e| e.to_string())?;
    }
    swaps.retain(|s| s.source != source);
    save_state(backups, &swaps)
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct SavedTexture {
    index: usize,
    name: String,
    before: String,
    after: String,
}
fn texture_backup(backups: &Path, source: usize, file: &str, index: usize) -> std::path::PathBuf {
    state_dir(backups).join(format!("{source}-{file}-{index}.texture"))
}
// Only verified diffuse maps. Multi-material/procedural balls need their full
// shader graph and are deliberately kept geometry-only for now.
pub fn colour_texture(source: usize, donor: usize) -> Option<&'static str> {
    if source != 0 {
        return None;
    }
    Some(match donor {
        1 => "HockeyPuck_D",
        2 => "eggball_RGBA",
        3 => "Ball_CubeBall00_D",
        4 => "Ball_Basketball_D",
        5 => "Ball_Beachball_D",
        8 => "Ball_PumpkinCube_D",
        9 => "Ball_Breakout_D",
        11 => "NikeBall_D",
        12 => "ball_adidas_d",
        13 => "SoccerBall_D",
        14 => "ball_luminousairplane_d",
        15 => "Ball_SphereGulp_D_2k",
        _ => return None,
    })
}
fn donor_colours(cooked: &Path, source: usize, donor: usize) -> Result<image::RgbaImage, String> {
    let name = colour_texture(source, donor)
        .ok_or("Colour merge is unavailable for this pair; use shape only")?;
    let p = UpkPackage::load(&cooked.join("Mutators_Balls_SF.upk"))?;
    let e = p
        .exports
        .iter()
        .find(|e| {
            strip(&p.name_of(e.object_name)) == name && p.class_of(e).starts_with("Texture2D")
        })
        .ok_or("Donor colour texture missing")?;
    let image = super::cosmetic_thumbnail::texture(&p, e, cooked)?;
    if image.width() < 256 || image.height() < 256 {
        return Err(
            "Only a low-resolution donor mip is readable; check the texture cache files".into(),
        );
    }
    Ok(image)
}
fn merge_colours(
    path: &Path,
    image: &image::RgbaImage,
    backups: &Path,
    source: usize,
    file: &str,
) -> Result<Vec<SavedTexture>, String> {
    let mut p = UpkPackage::load(path)?;
    let originals: Vec<_> = p
        .exports
        .iter()
        .map(|e| p.image[e.serial_offset..e.serial_offset + e.serial_size].to_vec())
        .collect();
    let mut changed = Vec::new();
    for name in [
        "Ball_Default00_D",
        "Ball_Default00_N",
        "Ball_Default00_RGB",
        "Ball_Default00_E",
    ] {
        let e = p
            .exports
            .iter()
            .find(|e| {
                strip(&p.name_of(e.object_name)) == name && p.class_of(e).starts_with("Texture2D")
            })
            .cloned();
        let Some(e) = e else {
            if name == "Ball_Default00_E" {
                continue;
            }
            return Err(format!("{file}: missing normal ball texture {name}"));
        };
        let neutral = image::RgbaImage::from_pixel(
            4,
            4,
            image::Rgba(if name.ends_with("_N") {
                [128, 128, 255, 255]
            } else {
                [0, 0, 0, 255]
            }),
        );
        let pixels = if name.ends_with("_D") {
            image
        } else {
            &neutral
        };
        let bytes = super::cosmetic_thumbnail::bake_texture(&p, &e, pixels)
            .map_err(|error| format!("{file}: {name}: {error}"))?;
        let before = &originals[e.table_index];
        std::fs::write(texture_backup(backups, source, file, e.table_index), before)
            .map_err(|e| e.to_string())?;
        changed.push(SavedTexture {
            index: e.table_index,
            name: name.into(),
            before: hash(before),
            after: hash(&bytes),
        });
        p.replace_export_payload(e.table_index, &bytes)?;
    }
    p.save(path)?;
    let check = UpkPackage::load(path)?;
    for e in &check.exports {
        let bytes = &check.image[e.serial_offset..e.serial_offset + e.serial_size];
        if let Some(t) = changed.iter().find(|t| t.index == e.table_index) {
            if hash(bytes) != t.after {
                return Err("Colour texture round-trip failed".into());
            }
            // Must decode from package alone, without any TFC files.
            super::cosmetic_thumbnail::texture(&check, e, &path.with_extension("no-cache"))
                .map_err(|error| format!("Check {}: {error}", t.name))?;
        } else if bytes != originals[e.table_index] {
            return Err("Colour merge changed an unrelated export".into());
        }
    }
    Ok(changed)
}
fn validate_textures(
    p: &UpkPackage,
    backups: &Path,
    source: usize,
    file: &SavedMesh,
) -> Result<(), String> {
    for t in &file.textures {
        let e = p
            .exports
            .get(t.index)
            .ok_or("Saved texture index is invalid")?;
        if strip(&p.name_of(e.object_name)) != t.name || !p.class_of(e).starts_with("Texture2D") {
            return Err("Saved texture identity changed".into());
        }
        let bytes = std::fs::read(texture_backup(backups, source, &file.file, t.index))
            .map_err(|e| e.to_string())?;
        if hash(&bytes) != t.before {
            return Err("Colour texture backup checksum failed".into());
        }
        let current = hash(&p.image[e.serial_offset..e.serial_offset + e.serial_size]);
        if current != t.before && current != t.after {
            return Err(format!(
                "{}: {} changed after this swap; restore stopped to preserve it",
                file.file, t.name
            ));
        }
    }
    Ok(())
}
