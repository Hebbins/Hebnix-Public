//! Lossless edits to saved inventory membership and equipped instance references.
//! Object numbers and unreferenced serialized objects are retained: renumbering
//! them would require interpreting every unrelated property in the save.
use super::crypto::{aes_decrypt, aes_encrypt, crc32};
use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;

type Result<T> = std::result::Result<T, String>;
const MAX_SAVE: usize = 64 * 1024 * 1024;

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct CleanupReport {
    pub inventory_instances: usize,
    pub inventory_memberships: usize,
    pub equipped_slots: usize,
    pub auxiliary_references: usize,
}
impl CleanupReport {
    pub fn changed(&self) -> bool {
        self.inventory_instances
            + self.inventory_memberships
            + self.equipped_slots
            + self.auxiliary_references
            > 0
    }
}
pub struct CleanupPlan {
    pub bytes: Vec<u8>,
    pub report: CleanupReport,
}

fn u32_at(data: &[u8], at: usize) -> Result<u32> {
    Ok(u32::from_le_bytes(
        data.get(at..at.checked_add(4).ok_or("offset overflow")?)
            .ok_or("truncated save")?
            .try_into()
            .unwrap(),
    ))
}
fn text(data: &[u8], at: usize) -> Result<(String, usize)> {
    let n = u32_at(data, at)? as i32;
    if n <= 0 || n > 1024 {
        return Err("unsupported property/type name encoding".into());
    }
    let end = at + 4 + n as usize;
    let bytes = data.get(at + 4..end).ok_or("truncated property name")?;
    if bytes.last() != Some(&0) {
        return Err("unterminated property name".into());
    }
    Ok((
        std::str::from_utf8(&bytes[..bytes.len() - 1])
            .map_err(|_| "invalid property name")?
            .into(),
        end,
    ))
}
#[derive(Clone)]
struct Field {
    name: String,
    tag: String,
    len_at: usize,
    payload: Range<usize>,
    struct_type: Option<String>,
}
fn fields(data: &[u8]) -> Result<(Vec<Field>, usize)> {
    let mut out = Vec::new();
    let mut at = 0;
    loop {
        let (name, next) = text(data, at)?;
        if name == "None" {
            return Ok((out, next));
        }
        let (tag, next) = text(data, next)?;
        let len = u32_at(data, next)? as usize;
        if len > MAX_SAVE || u32_at(data, next + 4)? != 0 {
            return Err("unsupported property length/index".into());
        }
        let mut start = next + 8;
        let mut struct_type = None;
        let actual = match tag.as_str() {
            "BoolProperty" => 1,
            "IntProperty" | "ObjectProperty" | "FloatProperty" if len == 4 => 4,
            "QWordProperty" if len == 8 => 8,
            "StructProperty" => {
                let (kind, pos) = text(data, start)?;
                start = pos;
                struct_type = Some(kind);
                len
            }
            "ByteProperty" => {
                let (_, pos) = text(data, start)?;
                start = pos;
                len
            }
            "StrProperty" | "NameProperty" | "ArrayProperty" => len,
            _ => return Err(format!("unsupported property tag/size: {tag}")),
        };
        let end = start
            .checked_add(actual)
            .filter(|end| *end <= data.len())
            .ok_or("property exceeds object")?;
        out.push(Field {
            name,
            tag,
            len_at: next,
            payload: start..end,
            struct_type,
        });
        if out.len() > 4096 {
            return Err("too many fields".into());
        }
        at = end;
    }
}
fn field<'a>(list: &'a [Field], name: &str, tag: &str) -> Result<&'a Field> {
    let matches: Vec<_> = list.iter().filter(|f| f.name == name).collect();
    if matches.len() != 1 || matches[0].tag != tag {
        return Err(format!("missing/ambiguous {name} {tag}"));
    }
    Ok(matches[0])
}
fn instance(data: &[u8]) -> Result<(String, usize, Vec<Range<usize>>)> {
    let (props, end) = fields(data)?;
    let mut upper = 0u64;
    let mut lower = 0u64;
    let mut seen = BTreeSet::new();
    let mut spans = Vec::new();
    for p in props {
        if p.tag != "QWordProperty" || !seen.insert(p.name.clone()) {
            return Err("invalid instance ID fields".into());
        }
        let value = u64::from_le_bytes(
            data[p.payload.clone()]
                .try_into()
                .map_err(|_| "invalid instance width")?,
        );
        match p.name.as_str() {
            "UpperBits" => upper = value,
            "LowerBits" => lower = value,
            _ => return Err("unknown instance ID field".into()),
        }
        spans.push(p.payload);
    }
    Ok((format!("{upper:016x}{lower:016x}"), end, spans))
}
fn zero_instance(data: &mut [u8]) -> Result<()> {
    let (_, end, spans) = instance(data)?;
    if end != data.len() {
        return Err("unexpected instance tail".into());
    }
    for span in spans {
        data[span].fill(0);
    }
    Ok(())
}
fn id_array(data: &[u8]) -> Result<Vec<(String, Range<usize>)>> {
    let count = u32_at(data, 0)? as usize;
    if count > 100_000 || count > data.len() / 9 {
        return Err("invalid instance array count".into());
    }
    let mut at = 4;
    let mut entries = Vec::new();
    for _ in 0..count {
        let (id, size, _) = instance(data.get(at..).ok_or("truncated instance array")?)?;
        entries.push((id, at..at + size));
        at += size;
    }
    if at != data.len() {
        return Err("instance array length mismatch".into());
    }
    Ok(entries)
}
fn int_array(data: &[u8]) -> Result<Vec<u32>> {
    let count = u32_at(data, 0)? as usize;
    if count > 100_000 || count.checked_mul(4).and_then(|n| n.checked_add(4)) != Some(data.len()) {
        return Err("integer array length mismatch".into());
    }
    (0..count).map(|i| u32_at(data, 4 + i * 4)).collect()
}
fn replace_field(data: &mut Vec<u8>, f: &Field, payload: &[u8]) {
    data[f.len_at..f.len_at + 4].copy_from_slice(&(payload.len() as u32).to_le_bytes());
    data.splice(f.payload.clone(), payload.iter().copied());
}
fn filter_ids(data: &mut Vec<u8>, name: &str, targets: &BTreeSet<String>) -> Result<usize> {
    let (props, _) = fields(data)?;
    if !props.iter().any(|p| p.name == name) {
        return Ok(0);
    }
    let f = field(&props, name, "ArrayProperty")?;
    let src = &data[f.payload.clone()];
    let entries = id_array(src)?;
    let kept: Vec<_> = entries
        .iter()
        .filter(|(id, _)| !targets.contains(id))
        .collect();
    let removed = entries.len() - kept.len();
    if removed == 0 {
        return Ok(0);
    }
    let mut payload = (kept.len() as u32).to_le_bytes().to_vec();
    for (_, span) in kept {
        payload.extend_from_slice(&src[span.clone()]);
    }
    replace_field(data, f, &payload);
    Ok(removed)
}
struct Object {
    kind: String,
    index: u32,
    bytes: Vec<u8>,
}
struct Document {
    header: Vec<u8>,
    root: Vec<u8>,
    objects: Vec<Object>,
    tail: Vec<u8>,
}
impl Document {
    fn read(raw: &[u8]) -> Result<Self> {
        if raw.len() > MAX_SAVE {
            return Err("save exceeds supported size".into());
        }
        let len = u32_at(raw, 0)? as usize;
        if len == 0 || len % 16 != 0 {
            return Err("invalid encrypted length".into());
        }
        let encrypted = raw
            .get(8..8usize.checked_add(len).ok_or("length overflow")?)
            .ok_or("truncated encrypted save")?;
        if crc32(encrypted) != u32_at(raw, 4)? {
            return Err("save CRC mismatch; original left unchanged".into());
        }
        let dec = aes_decrypt(encrypted);
        if u32_at(&dec, 0)? != 0xF005_BA11
            || u32_at(&dec, 4)? != 0x7FFF_FFFF
            || (u32_at(&dec, 8)?, u32_at(&dec, 12)?, u32_at(&dec, 16)?) != (868, 34, 0)
        {
            return Err("unsupported Rocket League save version".into());
        }
        let savedata_len = u32_at(&dec, 20)? as usize;
        if savedata_len < 8 {
            return Err("invalid savedata length".into());
        }
        let table_at = 20usize
            .checked_add(savedata_len)
            .filter(|at| *at <= dec.len())
            .ok_or("truncated savedata")?;
        let count = u32_at(&dec, table_at)? as usize;
        if count == 0 || count > 100_000 {
            return Err("invalid object count".into());
        }
        let mut at = table_at + 4;
        let mut table = Vec::new();
        let mut indices = BTreeSet::new();
        for _ in 0..count {
            let (kind, next) = text(&dec, at)?;
            let pos = u32_at(&dec, next)? as usize;
            let index = u32_at(&dec, next + 4)?;
            let start = 20usize
                .checked_add(pos)
                .filter(|n| *n >= 28 && *n < table_at)
                .ok_or("invalid object position")?;
            if index != table.len() as u32
                || !indices.insert(index)
                || table.last().is_some_and(|(_, last, _)| *last >= start)
            {
                return Err("unsupported object table ordering/index".into());
            }
            table.push((kind, start, index));
            at = next + 8;
        }
        if dec
            .get(at..)
            .is_none_or(|tail| tail.len() > 15 || tail.iter().any(|b| *b != 0))
        {
            return Err("unsupported encrypted save trailer".into());
        }
        if u32_at(&dec, 24)? != u32::MAX {
            return Err("invalid root marker".into());
        }
        let root = dec[28..table[0].1].to_vec();
        let (_, root_end) = fields(&root)?;
        if root_end != root.len() {
            return Err("unsupported root trailer".into());
        }
        let mut objects = Vec::new();
        for (i, (kind, start, index)) in table.iter().enumerate() {
            let end = table.get(i + 1).map_or(table_at, |v| v.1);
            if end < start + 4 {
                return Err("overlapping object marker".into());
            }
            if u32_at(&dec, *start)? != u32::MAX {
                return Err("invalid object marker".into());
            }
            objects.push(Object {
                kind: kind.clone(),
                index: *index,
                bytes: dec[start + 4..end].to_vec(),
            });
        }
        Ok(Self {
            header: dec[..24].to_vec(),
            root,
            objects,
            tail: raw[8 + len..].to_vec(),
        })
    }
    fn encode(&self) -> Result<Vec<u8>> {
        use super::binary_serializer::write_ue3;
        let mut dec = self.header.clone();
        dec.extend_from_slice(&u32::MAX.to_le_bytes());
        dec.extend_from_slice(&self.root);
        let mut positions = Vec::new();
        for o in &self.objects {
            positions.push((dec.len() - 20) as u32);
            dec.extend_from_slice(&u32::MAX.to_le_bytes());
            dec.extend_from_slice(&o.bytes);
        }
        let size = (dec.len() - 20) as u32;
        dec[20..24].copy_from_slice(&size.to_le_bytes());
        dec.extend_from_slice(&(self.objects.len() as u32).to_le_bytes());
        for (o, pos) in self.objects.iter().zip(positions) {
            dec.extend_from_slice(&write_ue3(&o.kind));
            dec.extend_from_slice(&pos.to_le_bytes());
            dec.extend_from_slice(&o.index.to_le_bytes());
        }
        let encrypted = aes_encrypt(&dec);
        let mut raw = (encrypted.len() as u32).to_le_bytes().to_vec();
        raw.extend_from_slice(&crc32(&encrypted).to_le_bytes());
        raw.extend_from_slice(&encrypted);
        raw.extend_from_slice(&self.tail);
        Ok(raw)
    }
    fn clean(&mut self, targets: &BTreeSet<String>) -> Result<CleanupReport> {
        let mut report = CleanupReport::default();
        let mut removed_objects = BTreeSet::new();
        let mut identities = BTreeMap::new();
        for o in &self.objects {
            if o.kind != "TAGame.OnlineProduct_TA" {
                continue;
            }
            let (props, end) = fields(&o.bytes)?;
            if end != o.bytes.len() {
                return Err("unsupported product object trailer".into());
            }
            let f = field(&props, "InstanceID", "StructProperty")?;
            if f.struct_type.as_deref() != Some("ProductInstanceID") {
                return Err("unsupported product instance type".into());
            }
            let (id, end, _) = instance(&o.bytes[f.payload.clone()])?;
            if end != f.payload.len() {
                return Err("unexpected product instance tail".into());
            }
            if targets.contains(&id) {
                if identities.insert(id, o.index).is_some() {
                    return Err("duplicate synthetic instance in save".into());
                }
                removed_objects.insert(o.index);
            }
        }
        let (props, _) = fields(&self.root)?;
        let f = field(&props, "OnlineProducts", "ArrayProperty")?;
        let entries = int_array(&self.root[f.payload.clone()])?;
        if entries.iter().any(|index| {
            self.objects
                .get(*index as usize)
                .is_none_or(|o| o.kind != "TAGame.OnlineProduct_TA")
        }) {
            return Err("invalid root inventory object reference".into());
        }
        let kept: Vec<_> = entries
            .iter()
            .filter(|id| !removed_objects.contains(id))
            .collect();
        report.inventory_instances = entries.len() - kept.len();
        if report.inventory_instances > 0 {
            let mut payload = (kept.len() as u32).to_le_bytes().to_vec();
            for id in kept {
                payload.extend_from_slice(&id.to_le_bytes());
            }
            replace_field(&mut self.root, f, &payload);
        }
        for o in &mut self.objects {
            match o.kind.as_str() {
                "TAGame.ProductsSave_TA" => {
                    report.inventory_memberships +=
                        filter_ids(&mut o.bytes, "OnlineProductInstanceIDs128", targets)?;
                    let (props, _) = fields(&o.bytes)?;
                    if props.iter().any(|p| p.name == "LastUnlockDisplayId128") {
                        let f = field(&props, "LastUnlockDisplayId128", "StructProperty")?;
                        if f.struct_type.as_deref() != Some("ProductInstanceID") {
                            return Err("unsupported last-unlock instance type".into());
                        }
                        if targets.contains(&instance(&o.bytes[f.payload.clone()])?.0) {
                            zero_instance(&mut o.bytes[f.payload.clone()])?;
                            report.auxiliary_references += 1;
                        }
                    }
                }
                "TAGame.ProductsFavoriteSave_TA" | "TAGame.ProductsArchiveSave_TA" => {
                    report.auxiliary_references +=
                        filter_ids(&mut o.bytes, "InstanceIDs128", targets)?;
                }
                "TAGame.Loadout_TA" => {
                    let (props, _) = fields(&o.bytes)?;
                    if !props.iter().any(|p| p.name == "OnlineProducts128") {
                        continue;
                    }
                    let ids = field(&props, "OnlineProducts128", "ArrayProperty")?;
                    let products = field(&props, "Products", "ArrayProperty")?;
                    let entries = id_array(&o.bytes[ids.payload.clone()])?;
                    let products_values = int_array(&o.bytes[products.payload.clone()])?;
                    if entries.len() != products_values.len() {
                        return Err("loadout product/instance length mismatch".into());
                    }
                    for (slot, (id, span)) in entries.into_iter().enumerate() {
                        if !targets.contains(&id) {
                            continue;
                        }
                        zero_instance(
                            &mut o.bytes
                                [ids.payload.start + span.start..ids.payload.start + span.end],
                        )?;
                        let at = products.payload.start + 4 + slot * 4;
                        o.bytes[at..at + 4].fill(0);
                        report.equipped_slots += 1;
                    }
                }
                _ => {}
            }
        }
        Ok(report)
    }
}

/// Plan only: the caller owns process exclusion, backups and atomic installation.
pub fn plan_cleanup(raw: &[u8], targets: &BTreeSet<String>) -> Result<CleanupPlan> {
    if targets
        .iter()
        .any(|id| id.len() != 32 || !id.bytes().all(|b| b.is_ascii_hexdigit()))
    {
        return Err("invalid synthetic InstanceID".into());
    }
    let targets: BTreeSet<_> = targets.iter().map(|id| id.to_ascii_lowercase()).collect();
    let mut doc = Document::read(raw)?;
    // A no-op codec round-trip must reproduce every byte before editing anything.
    if doc.encode()? != raw {
        return Err("save does not round-trip losslessly; original left unchanged".into());
    }
    let report = doc.clean(&targets)?;
    if !report.changed() {
        return Ok(CleanupPlan {
            bytes: raw.to_vec(),
            report,
        });
    }
    let bytes = doc.encode()?;
    let mut checked = Document::read(&bytes)?;
    if checked.clean(&targets)?.changed() || checked.encode()? != bytes {
        return Err("cleanup verification failed".into());
    }
    Ok(CleanupPlan { bytes, report })
}

