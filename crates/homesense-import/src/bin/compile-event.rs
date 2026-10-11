//! Offline event fragment conversion; no transport or runtime activation.
use std::{collections::BTreeMap,io::Read};
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Binding { device:String, kind:String }
fn read(path:&str,limit:u64)->Result<String,String> {
    let mut value=String::new();
    std::fs::File::open(path).map_err(|e|e.to_string())?.take(limit+1).read_to_string(&mut value).map_err(|e|e.to_string())?;
    if value.len() as u64>limit {return Err("input exceeds size limit".into());}
    Ok(value)
}
fn run()->Result<(),String> {
    let args=std::env::args().skip(1).collect::<Vec<_>>();
    if args.len()!=4 {return Err("usage: compile-event EVENT.xml BINDINGS.json INTERVAL_MS LATENESS_MS".into());}
    let xml=read(&args[0],4*1024*1024)?;
    let inputs:BTreeMap<String,Binding>=serde_json::from_str(&read(&args[1],1024*1024)?).map_err(|e|e.to_string())?;
    let bindings=inputs.into_iter().map(|(id,binding)|{
        use homesense_core::ModuleKind;
        let kind=match binding.kind.as_str() {"switch"=>ModuleKind::Switch,"dimmer"=>ModuleKind::Dimmer,"motor"=>ModuleKind::Motor,"keypad"=>ModuleKind::Keypad,_=>return Err("unknown binding kind".to_owned())};
        Ok((id,homesense_import::semantic::Binding{device:homesense_core::DeviceId::new(binding.device).map_err(|e|e.to_string())?,kind}))
    }).collect::<Result<BTreeMap<_,_>,String>>()?;
    let compiled=homesense_import::semantic::compile_event_with_fades(&xml,&bindings,args[2].parse().map_err(|_|"invalid interval")?,args[3].parse().map_err(|_|"invalid lateness")?)?;
    let draft=homesense_import::event_export::draft(&compiled)?;
    println!("{}",serde_json::to_string_pretty(&draft).map_err(|e|e.to_string())?);
    Ok(())
}
fn main() {if let Err(error)=run(){eprintln!("{error}");std::process::exit(1);}}
