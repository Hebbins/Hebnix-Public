#![allow(dead_code, unused_imports)]
#[path = "../src/patcher/patch_core.rs"] pub mod patch_core;
#[path = "../src/patcher/upk_package.rs"] pub mod upk_package;
#[path = "../src/patcher/upk_keys.rs"] mod upk_keys;
#[path = "../src/patcher/cosmetic_thumbnail.rs"] mod cosmetic_thumbnail;
mod patcher { pub use crate::{patch_core, upk_package}; }
#[path = "support/car_geometry.rs"] mod car_geometry;
#[path = "support/evo_prepared.rs"] mod evo_prepared;
use upk_package::{UpkPackage, strip};
fn main() -> Result<(), String> {
    let args: Vec<_> = std::env::args().collect();
    if args.get(1).map(String::as_str)==Some("build") {
        if args.len()!=5 {return Err("Usage: prepare_evo build <stock-Endo> <EVO-donor> <new-output>".into());}
        return evo_prepared::build(args[2].as_ref(),args[3].as_ref(),args[4].as_ref());
    }
    for path in &args[1..] {
        let p = UpkPackage::load(std::path::Path::new(path))?;
        println!("PACKAGE {path}");
        for e in &p.exports {
            let class = p.class_of(e);
            if strip(&class)=="ProductAsset_Body_TA" {
                for prop in p.serialized_props(e)?.0 {println!("BODYPROP {} {} {} {:?}",prop.name,prop.tag_type,prop.size,&p.image[e.serial_offset+prop.value_offset..e.serial_offset+prop.value_offset+prop.size.min(48)]);}
            }
            if ["MaterialInstanceConstant", "Texture2D", "SkeletalMeshSocket"].contains(&strip(&class)) {
                println!("ASSET {} {} {}",e.table_index+1,strip(&class),p.name_of(e.object_name));
                if strip(&class) != "Texture2D" {
                    for prop in p.serialized_props(e)?.0 {
                        let at=e.serial_offset+prop.value_offset;
                        if prop.tag_type=="ObjectProperty" {println!("  {} -> {}",prop.name,p.object_path(p.read_int(at)?));}
                        if prop.name.ends_with("ParameterValues") {
                            let n=p.read_int(at)?; let mut pos=at+4;
                            for _ in 0..n { let (fields,next)=p.nested_props(pos,at+prop.size)?; pos=next; for f in fields { if f.tag_type=="NameProperty" {println!("    {} {}",f.name,p.names[p.read_int(f.value_offset)? as usize]);} else if f.tag_type=="ObjectProperty" {println!("    {} -> {}",f.name,p.object_path(p.read_int(f.value_offset)?));} } }
                        }
                    }
                }
            }
            if strip(&class) != "SkeletalMesh" { continue; }
            let m = car_geometry::inspect(&p, e)?;
            let d = &p.image[e.serial_offset..e.serial_offset+e.serial_size];
            println!("MESH {} native={} tail={} size={} bones={:?}",p.name_of(e.object_name),m.native,m.tail,d.len(),m.bones);
            println!("native prefix {:?}", &d[m.native..m.native+48]);
            let (props, _) = p.serialized_props(e)?;
            for prop in &props { if prop.name=="LODInfo" { let at=e.serial_offset+prop.value_offset; let (nested,_) =p.nested_props(at+4,at+prop.size)?; for v in nested {println!("LODPROP {} {} {} {:?}",v.name,v.tag_type,v.size,&p.image[v.value_offset..v.value_offset+v.size.min(40)]);}} }
            for prop in props { println!("PROP {} {} size={} bool={:?} value={:?}",prop.name,prop.tag_type,prop.size,prop.bool_value,&d[prop.value_offset..prop.value_offset+prop.size.min(120)]); }
            for lod in m.lods {
                let mut r = car_geometry::Reader{data:d,at:lod.start};
                let s = r.array(13)?; println!("sections {:?}",&d[s]);
                let ix = r.indices()?; println!("indices {}",ix.len());
                let a = r.array(2)?; println!("active {:?}",&d[a]);
                let n = r.count()?;
                for _ in 0..n { let base = r.int()?; let rigid=r.array(61)?; let soft=r.array(68)?; let bones=r.array(2)?; let nr=r.int()?; let ns=r.int()?; let ni=r.int()?; println!("chunk base={base} rigid={} soft={} bones={:?} counts={nr}/{ns}/{ni}",rigid.len(),soft.len(),&d[bones]); }
                println!("size={} vertices={}",r.int()?,r.int()?);
                let req=r.array(1)?; println!("required {:?}",&d[req]);
                let f=r.int()?; let count=r.count()?; let bytes=r.count()?;
                println!("bulk flags={f} count={count} bytes={bytes}");
                if f & 65536 == 0 {r.take(if u16::from_le_bytes(p.image[6..8].try_into().unwrap())>=22 {8}else{4})?; r.take(bytes)?;}
                let uv=r.count()?; let h=r.take(36)?; let (stride,v)=r.bulk()?;
                println!("GPU uv={uv} header={:?} stride={stride} vertices={}",&d[h],v.len()/stride);
                println!("remaining={} first={:?}",lod.end-r.at,&d[r.at..(r.at+80).min(lod.end)]);
            }
        }
    }
    Ok(())
}
