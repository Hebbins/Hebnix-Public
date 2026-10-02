//! Bounded UE3 skeletal LOD inspector and transplant used by the car patcher.
use crate::patcher::upk_package::{ExportEntry, UpkPackage, strip};
use std::ops::Range;

pub struct Reader<'a> {
    pub data: &'a [u8],
    pub at: usize,
}

impl Reader<'_> {
    pub fn take(&mut self, n: usize) -> Result<Range<usize>, String> {
        let end = self.at.checked_add(n).ok_or("Overflow")?;
        if end > self.data.len() {
            return Err(format!("Truncated mesh at {} + {n}", self.at));
        }
        let span = self.at..end;
        self.at = end;
        Ok(span)
    }

    pub fn int(&mut self) -> Result<i32, String> {
        let range = self.take(4)?;
        Ok(i32::from_le_bytes(self.data[range].try_into().unwrap()))
    }

    pub fn count(&mut self) -> Result<usize, String> {
        let at = self.at;
        let n = self.int()?;
        if !(0..=2_000_000).contains(&n) {
            return Err(format!("Invalid count {n} at {at}"));
        }
        Ok(n as usize)
    }

    pub fn array(&mut self, stride: usize) -> Result<Range<usize>, String> {
        let n = self.count()?;
        self.take(n.checked_mul(stride).ok_or("Array overflow")?)
    }

    pub fn bulk(&mut self) -> Result<(usize, Range<usize>), String> {
        let stride = self.count()?;
        if stride > 256 {
            return Err(format!("Invalid stride {stride} at {}", self.at - 4));
        }
        let data = self.array(stride)?;
        Ok((stride, data))
    }

    pub fn indices(&mut self) -> Result<Range<usize>, String> {
        self.int()?;
        let width = self.data[self.take(1)?][0] as usize;
        let (stride, data) = self.bulk()?;
        if stride != width || ![2, 4].contains(&width) {
            return Err("Index stride mismatch".into());
        }
        Ok(data)
    }
}

pub struct Mesh {
    pub native: usize,
    pub lod_start: usize,
    pub tail: usize,
    pub bones: Vec<String>,
    pub lods: Vec<Range<usize>>,
}

pub fn inspect(package: &UpkPackage, export: &ExportEntry) -> Result<Mesh, String> {
    let (_, native) = package.serialized_props(export)?;
    let data = &package.image[export.serial_offset..export.serial_offset + export.serial_size];
    let mut reader = Reader { data, at: native };
    reader.take(28)?;
    reader.array(4)?;
    reader.take(24)?;
    let bone_count = reader.count()?;
    let mut bones = Vec::with_capacity(bone_count);
    for _ in 0..bone_count {
        let index = reader.count()?;
        bones.push(
            package
                .names
                .get(index)
                .ok_or("Bone name out of bounds")?
                .clone(),
        );
        reader.take(48)?;
    }
    reader.int()?;
    let lod_start = reader.at;
    let lod_count = reader.count()?;
    if lod_count > 8 {
        return Err("Too many LODs".into());
    }
    let licensee = u16::from_le_bytes(package.image[6..8].try_into().unwrap());
    let mut lods = Vec::with_capacity(lod_count);
    for _ in 0..lod_count {
        let start = reader.at;
        reader.array(13)?;
        reader.indices()?;
        reader.array(2)?;
        let chunks = reader.count()?;
        for _ in 0..chunks {
            reader.int()?;
            reader.array(61)?;
            reader.array(68)?;
            reader.array(2)?;
            reader.take(12)?;
        }
        let _size = reader.int()?;
        let vertices = reader.count()?;
        reader.array(1)?;
        let flags = reader.int()?;
        reader.count()?;
        let bytes = reader.count()?;
        if flags & 65_536 == 0 {
            reader.take(if licensee >= 22 { 8 } else { 4 })?;
            reader.take(bytes)?;
        }
        reader.count()?;
        reader.take(36)?;
        let (stride, vertex_data) = reader.bulk()?;
        if stride == 0 || vertex_data.len() / stride != vertices {
            return Err("GPU vertex count mismatch".into());
        }
        let extra_stride = reader.count()?;
        if extra_stride > 0 {
            if extra_stride > 16 {
                return Err("Extra vertex influence stride is implausible".into());
            }
            let extra = reader.array(extra_stride)?;
            if extra.len() / extra_stride != vertices {
                return Err("Extra vertex influence count mismatch".into());
            }
        }
        if extra_stride == 0 {
            reader.indices()?;
        }
        lods.push(start..reader.at);
    }
    Ok(Mesh {
        native,
        lod_start,
        tail: reader.at,
        bones,
        lods,
    })
}

fn put(out: &mut Vec<u8>, value: usize) {
    out.extend_from_slice(&(value as u32).to_le_bytes());
}

fn write_bones(out: &mut Vec<u8>, bones: &[u16]) {
    put(out, bones.len());
    for bone in bones {
        out.extend_from_slice(&bone.to_le_bytes());
    }
}

/// Embed a validated standalone CustomCar mesh into a stock body package.
pub fn transplant_profile(
    stock: &std::path::Path,
    donor: &std::path::Path,
    output: &std::path::Path,
    target_mesh: &str,
    donor_mesh: &str,
    material_map: &[u16],
) -> Result<(), String> {
    if output.exists() {
        return Err("Use a new staging destination".into());
    }
    let mut target = UpkPackage::load(stock)?;
    let donor = UpkPackage::load(donor)?;
    let target_export = target
        .exports
        .iter()
        .find(|export| {
            strip(&target.class_of(export)) == "SkeletalMesh"
                && strip(&target.name_of(export.object_name)) == target_mesh
        })
        .ok_or("Missing target mesh")?
        .clone();
    let donor_export = donor
        .exports
        .iter()
        .find(|export| {
            strip(&donor.class_of(export)) == "SkeletalMesh"
                && strip(&donor.name_of(export.object_name)) == donor_mesh
        })
        .ok_or("Missing donor mesh")?
        .clone();
    let target_mesh_info = inspect(&target, &target_export)?;
    let donor_mesh_info = inspect(&donor, &donor_export)?;
    if target_mesh_info.lods.len() != 1 || donor_mesh_info.lods.len() != 1 {
        return Err("Only inspected single-LOD pairs are supported".into());
    }
    let target_bytes = target.image
        [target_export.serial_offset..target_export.serial_offset + target_export.serial_size]
        .to_vec();
    let donor_bytes = &donor.image
        [donor_export.serial_offset..donor_export.serial_offset + donor_export.serial_size];

    let mut bone_map = Vec::with_capacity(donor_mesh_info.bones.len());
    for name in &donor_mesh_info.bones {
        let candidate = name.strip_suffix("_end").unwrap_or(name);
        let index = target_mesh_info
            .bones
            .iter()
            .position(|target_name| target_name == candidate)
            .or_else(|| name.ends_with("_SK_ao").then_some(0))
            .ok_or_else(|| format!("Unmapped bone {name}"))?;
        bone_map.push(index as u16);
    }
    let map_bone = |old: u16| -> Result<u16, String> {
        bone_map
            .get(old as usize)
            .copied()
            .ok_or("Bone reference outside donor skeleton".into())
    };

    let mut reader = Reader {
        data: donor_bytes,
        at: donor_mesh_info.lods[0].start,
    };
    let section_span = reader.array(13)?;
    let mut sections = donor_bytes[section_span].to_vec();
    let index_start = reader.at;
    let index_data = reader.indices()?;
    let index_end = reader.at;
    let active_bones = reader.array(2)?;

    let mut out = target_bytes[..target_mesh_info.lod_start].to_vec();
    out[target_mesh_info.native..target_mesh_info.native + 28]
        .copy_from_slice(&donor_bytes[donor_mesh_info.native..donor_mesh_info.native + 28]);
    put(&mut out, 1);
    put(&mut out, sections.len() / 13);
    for section in sections.chunks_exact_mut(13) {
        let material = u16::from_le_bytes(section[..2].try_into().unwrap());
        let mapped = *material_map
            .get(material as usize)
            .ok_or("The profile has no target mapping for a donor material slot")?;
        section[..2].copy_from_slice(&mapped.to_le_bytes());
    }
    out.extend_from_slice(&sections);
    out.extend_from_slice(&donor_bytes[index_start..index_end]);
    let mut mapped_active = donor_bytes[active_bones]
        .chunks_exact(2)
        .map(|bytes| map_bone(u16::from_le_bytes(bytes.try_into().unwrap())))
        .collect::<Result<Vec<_>, _>>()?;
    mapped_active.sort_unstable();
    mapped_active.dedup();
    write_bones(&mut out, &mapped_active);

    let chunks = reader.count()?;
    put(&mut out, chunks);
    let mut chunk_ranges = Vec::with_capacity(chunks);
    for _ in 0..chunks {
        let first = reader.count()?;
        put(&mut out, first);
        reader.array(61)?;
        reader.array(68)?;
        put(&mut out, 0);
        put(&mut out, 0);
        let bones = reader.array(2)?;
        let mapped = donor_bytes[bones]
            .chunks_exact(2)
            .map(|bytes| map_bone(u16::from_le_bytes(bytes.try_into().unwrap())))
            .collect::<Result<Vec<_>, _>>()?;
        write_bones(&mut out, &mapped);
        let rigid = reader.count()?;
        let soft = reader.count()?;
        let influences = reader.count()?;
        if influences > 4 {
            return Err("Unsupported skin influences".into());
        }
        for value in [rigid, soft, influences] {
            put(&mut out, value);
        }
        chunk_ranges.push((first, rigid + soft, mapped.len()));
    }

    let size = reader.count()?;
    let vertices = reader.count()?;
    put(&mut out, size);
    put(&mut out, vertices);
    let required = reader.array(1)?;
    for bone in &donor_bytes[required] {
        map_bone(*bone as u16)?;
    }
    put(&mut out, target_mesh_info.bones.len());
    out.extend((0..target_mesh_info.bones.len()).map(|index| index as u8));
    let flags = reader.int()?;
    reader.count()?;
    let bytes = reader.count()?;
    if flags != 0 {
        return Err("Unexpected donor source bulk flags".into());
    }
    reader.take(4)?;
    reader.take(bytes)?;
    for value in [65_536, 0, 0] {
        put(&mut out, value);
    }
    let uv = reader.count()?;
    let gpu_header = reader.take(36)?;
    let (stride, vertex_data) = reader.bulk()?;
    if !matches!((uv, stride), (1, 32) | (2, 36)) || vertex_data.len() / stride != vertices {
        return Err("Unsupported donor GPU format".into());
    }
    put(&mut out, 2);
    let mut header = donor_bytes[gpu_header].to_vec();
    header[..4].copy_from_slice(&2u32.to_le_bytes());
    out.extend_from_slice(&header);
    put(&mut out, 36);
    put(&mut out, vertices);
    for (index, vertex) in donor_bytes[vertex_data].chunks_exact(stride).enumerate() {
        let (_, _, bone_count) = chunk_ranges
            .iter()
            .find(|(first, count, _)| index >= *first && index < first + count)
            .ok_or("Vertex outside chunks")?;
        for influence in 0..4 {
            if vertex[12 + influence] > 0 && vertex[8 + influence] as usize >= *bone_count {
                return Err(format!(
                    "GPU bone index outside chunk: vertex={index} influence={influence} index={} weight={} chunk_bones={bone_count}",
                    vertex[8 + influence],
                    vertex[12 + influence]
                ));
            }
        }
        for position in vertex[16..28].chunks_exact(4) {
            if !f32::from_le_bytes(position.try_into().unwrap()).is_finite() {
                return Err("Non-finite position".into());
            }
        }
        out.extend_from_slice(vertex);
        if uv == 1 {
            out.extend_from_slice(&vertex[28..32]);
        }
    }
    let tail_start = reader.at;
    let extra_stride = reader.count()?;
    if extra_stride > 0 {
        if extra_stride > 16 {
            return Err("Extra vertex influence stride is implausible".into());
        }
        let extra = reader.array(extra_stride)?;
        if extra.len() / extra_stride != vertices {
            return Err("Extra vertex influence count mismatch".into());
        }
    }
    if extra_stride == 0 {
        reader.indices()?;
    }
    if reader.at != donor_mesh_info.tail {
        return Err("LOD parser disagreement".into());
    }
    out.extend_from_slice(&donor_bytes[tail_start..reader.at]);
    out.extend_from_slice(&target_bytes[target_mesh_info.tail..]);

    let width = donor_bytes[index_start + 4] as usize;
    for index in donor_bytes[index_data.clone()].chunks_exact(width) {
        let value = if width == 2 {
            u16::from_le_bytes(index.try_into().unwrap()) as usize
        } else {
            u32::from_le_bytes(index.try_into().unwrap()) as usize
        };
        if value >= vertices {
            return Err("Draw index outside vertices".into());
        }
    }
    for section in sections.chunks_exact(13) {
        let chunk = u16::from_le_bytes(section[2..4].try_into().unwrap()) as usize;
        let first = u32::from_le_bytes(section[4..8].try_into().unwrap()) as usize;
        let triangles = u32::from_le_bytes(section[8..12].try_into().unwrap()) as usize;
        if chunk >= chunks || first + triangles * 3 > index_data.len() / width {
            return Err("Invalid draw section".into());
        }
    }
    let originals = target
        .exports
        .iter()
        .map(|export| {
            target.image[export.serial_offset..export.serial_offset + export.serial_size].to_vec()
        })
        .collect::<Vec<_>>();
    target.replace_export_payload(target_export.table_index, &out)?;
    target.save(output)?;
    let check = UpkPackage::load(output)?;
    let check_export = &check.exports[target_export.table_index];
    inspect(&check, check_export)?;
    for (index, export) in check.exports.iter().enumerate() {
        let expected = if index == target_export.table_index {
            &out
        } else {
            &originals[index]
        };
        if check.image[export.serial_offset..export.serial_offset + export.serial_size]
            != expected[..]
        {
            return Err(format!("Export {index} round-trip mismatch"));
        }
    }
    Ok(())
}
