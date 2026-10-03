//! Prepare EVO using resident textures and Endo's cooked asset/skeleton.
use crate::{car_geometry, cosmetic_thumbnail, upk_package::{UpkPackage, ExportEntry, strip}};
use std::{path::Path, collections::HashSet};
use image::{Rgba, RgbaImage};

fn find(p:&UpkPackage, name:&str)->Result<usize,String>{
    p.exports.iter().position(|e|strip(&p.name_of(e.object_name))==name).ok_or_else(||format!("Missing {name}"))
}
fn payload(p:&UpkPackage,i:usize)->Vec<u8>{let e=&p.exports[i];p.image[e.serial_offset..e.serial_offset+e.serial_size].to_vec()}
fn bake(p:&mut UpkPackage,i:usize,pixels:&RgbaImage)->Result<(),String>{
    let bytes=cosmetic_thumbnail::bake_texture_alpha(p,&p.exports[i],pixels)?;
    p.replace_export_payload(i,&bytes)
}
fn donor_texture(d:&UpkPackage,dir:&Path,name:&str)->Result<RgbaImage,String>{cosmetic_thumbnail::texture(d,&d.exports[find(d,name)?],dir)}
fn set_texture(p:&mut UpkPackage,material:usize,parameter:&str,reference:i32)->Result<(),String>{
    let e=p.exports[material].clone();
    let (props,_)=p.serialized_props(&e)?;
    let prop=props.iter().find(|v|v.name=="TextureParameterValues").ok_or("No texture parameters")?;
    let start=e.serial_offset+prop.value_offset;
    let mut at=start+4;
    for _ in 0..p.read_int(start)? {
        let (fields,next)=p.nested_props(at,start+prop.size)?;at=next;
        let n=fields.iter().find(|f|f.name=="ParameterName").ok_or("No parameter name")?;
        if p.names[p.read_int(n.value_offset)? as usize]==parameter {
            let v=fields.iter().find(|f|f.name=="ParameterValue").ok_or("No parameter value")?;
            return p.patch_i32(v.value_offset,reference);
        }
    }
    Err(format!("Missing texture parameter {parameter}"))
}
fn tag_with_value(p:&UpkPackage,e:&ExportEntry,prop:&crate::upk_package::Prop,value:&[u8])->Vec<u8>{
    let mut tag=p.image[e.serial_offset+prop.tag_offset..e.serial_offset+prop.value_offset].to_vec();
    tag[16..20].copy_from_slice(&(value.len() as u32).to_le_bytes());tag.extend_from_slice(value);tag
}
fn section_metadata(p:&mut UpkPackage,mesh:usize)->Result<(),String>{
    let e=p.exports[mesh].clone();let (props,native)=p.serialized_props(&e)?;
    let old=payload(p,mesh); let mut out=old[..4].to_vec();
    for prop in &props {
        if prop.name=="ClothingAssets" {let mut v=7u32.to_le_bytes().to_vec();v.resize(32,0);out.extend(tag_with_value(p,&e,prop,&v));}
        else if prop.name=="LODInfo" {
            let start=e.serial_offset+prop.value_offset;
            if p.read_int(start)?!=1 {return Err("Expected one LODInfo".into());}
            let (fields,end)=p.nested_props(start+4,start+prop.size)?;
            let mut v=1u32.to_le_bytes().to_vec();
            for f in fields {
                let mut tag=p.image[f.tag_offset..f.value_offset].to_vec();
                let value=if f.name=="bEnableShadowCasting" {let mut b=6u32.to_le_bytes().to_vec();b.extend([1;6]);b}
                else if f.name=="TriangleSortSettings" {
                    let (_,one_end)=p.nested_props(f.value_offset+4,f.value_offset+f.size)?;
                    let one=&p.image[f.value_offset+4..one_end];let mut b=6u32.to_le_bytes().to_vec();for _ in 0..6 {b.extend_from_slice(one);}b
                } else {p.image[f.value_offset..f.value_offset+f.size].to_vec()};
                tag[16..20].copy_from_slice(&(value.len() as u32).to_le_bytes());tag.extend(value);v.extend(tag);
            }
            v.extend_from_slice(&p.image[end-8..end]);out.extend(tag_with_value(p,&e,prop,&v));
        } else {out.extend_from_slice(&old[prop.tag_offset..prop.value_offset+prop.size]);}
    }
    out.extend_from_slice(&old[native-8..]);p.replace_export_payload(mesh,&out)
}
pub fn build(stock:&Path,donor:&Path,output:&Path)->Result<(),String>{
    if output.exists(){return Err("Output already exists".into());}
    let staging=output.with_extension("geometry.upk");
    car_geometry::transplant_profile(stock,donor,&staging,"Body_Endo_SK","EVO_5F_SK",&[2,3,4,0,5,6])?;
    let mut p=UpkPackage::load(&staging)?;let d=UpkPackage::load(donor)?;
    let original=UpkPackage::load(stock)?;let dir=donor.parent().ok_or("No donor directory")?;
    let mut changed=HashSet::new();let mut textures=Vec::new();
    for (target,source) in [("Endo_Body_D","evo_diffuse1"),("Endo_Body_Curvature","evo_curvature1"),("Endo_Body_BlankSkin","evo_skin1"),("Endo_Chassis_D","Engine_Diffuse"),("Endo_Chassis_N","Engine_Normal"),("Endo_Chassis_RGB","Engine_Mask")] {
        let i=find(&p,target)?;bake(&mut p,i,&donor_texture(&d,dir,source)?)?;changed.insert(i);textures.push(i);
    }
    let chassis=find(&p,"MIC_Chassis_Endo")?;let paint=find(&p,"MIC_Body_Endo")?;
    let diffuse=find(&p,"Endo_Chassis_D")?;let normal=find(&p,"Endo_Chassis_N")?;let masks=find(&p,"Endo_Chassis_RGB")?;
    let mut new_mats=Vec::new();
    for (diff,norm,color) in [(Some("Body_2_Diffuse"),Some("Body_2_Normal"),[255,255,255,255]),(Some("Engine_2_Diffuse"),Some("Engine_2_Normal"),[255,255,255,255]),(None,None,[12,17,22,255]),(Some("nmbrplate_0"),None,[255,255,255,255])] {
        let mat=p.clone_into_empty_export(chassis)?;changed.insert(mat);new_mats.push(mat as i32+1);
        for (parameter,template,source,fallback) in [("Diffuse",diffuse,diff,color),("Normal",normal,norm,[128,128,255,255]),("Masks",masks,None,[0,0,0,255])] {
            let tex=p.clone_into_empty_export(template)?;
            let pixels=match source {Some(name)=>donor_texture(&d,dir,name)?,None=>RgbaImage::from_pixel(4,4,Rgba(fallback))};
            bake(&mut p,tex,&pixels)?;set_texture(&mut p,mat,parameter,tex as i32+1)?;
            changed.insert(tex);textures.push(tex);
        }
    }
    let mesh=find(&p,"Body_Endo_SK")?;changed.insert(mesh);
    let info=car_geometry::inspect(&p,&p.exports[mesh])?;let mut bytes=payload(&p,mesh);
    let material_start=info.native+28;
    let old_count=u32::from_le_bytes(bytes[material_start..material_start+4].try_into().unwrap()) as usize;
    if old_count!=3 {return Err("Expected three stock Endo materials".into());}
    let headlight=i32::from_le_bytes(bytes[material_start+8..material_start+12].try_into().unwrap());
    let mut refs=7u32.to_le_bytes().to_vec();for r in [chassis as i32+1,headlight,paint as i32+1].into_iter().chain(new_mats) {refs.extend(r.to_le_bytes());}
    bytes.splice(material_start..material_start+4+old_count*4,refs);p.replace_export_payload(mesh,&bytes)?;
    section_metadata(&mut p,mesh)?;
    // Wheel translations are relative to root_jnt in both reference skeletons.
    let info=car_geometry::inspect(&p,&p.exports[mesh])?;let mut bytes=payload(&p,mesh);
    let bone_start=info.native+28+4+7*4+24+4;
    for (name,position) in [("FL_WheelTranslation_jnt",[52.063118f32,-24.679306,-6.3058085]),("FR_WheelTranslation_jnt",[52.063118,24.641638,-6.3058085]),("BL_WheelTranslation_jnt",[-37.684216,-26.595377,-5.8298016]),("BR_WheelTranslation_jnt",[-37.684216,26.534807,-5.829776])] {
        let i=info.bones.iter().position(|b|b==name).ok_or("Missing wheel anchor")?;
        let at=bone_start+i*52+28;for (axis,v) in position.into_iter().enumerate(){bytes[at+axis*4..at+axis*4+4].copy_from_slice(&v.to_le_bytes());}
    }
    p.replace_export_payload(mesh,&bytes)?;
    let body=p.exports.iter().position(|e|strip(&p.class_of(e))=="ProductAsset_Body_TA").ok_or("Missing Endo body asset")?;
    let e=p.exports[body].clone();
    for prop in p.serialized_props(&e)?.0 {
        if prop.name=="FrontAxle" || prop.name=="BackAxle" {
            let at=e.serial_offset+prop.value_offset;
            for f in p.nested_props(at,at+prop.size)?.0 {
                println!("{} {} {:?}",prop.name,f.name,&p.image[f.value_offset..f.value_offset+f.size]);
                if f.name=="WheelScale" && f.size==4 {p.patch(f.value_offset,&0.75f32.to_le_bytes())?;changed.insert(body);}
            }
        }
    }
    p.save(output)?;let check=UpkPackage::load(output)?;
    let mesh_info=car_geometry::inspect(&check,&check.exports[mesh])?;
    if mesh_info.bones.len()!=38 {return Err("Endo skeleton was not retained".into());}
    for i in &textures {cosmetic_thumbnail::validate_resident_texture(&check,&check.exports[*i])?;}
    for i in 0..original.exports.len() {
        if !changed.contains(&i) && payload(&original,i)!=payload(&check,i){return Err(format!("Unrelated export {i} changed"));}
        if payload(&p,i)!=payload(&check,i){return Err(format!("Round-trip mismatch {i}"));}
    }
    println!("Prepared {}: {} resident textures, 7 material slots, 38 Endo bones; unrelated exports unchanged. In-game validation pending.",output.display(),textures.len());
    Ok(())
}
