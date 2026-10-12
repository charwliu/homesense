//! Fixed backup roots, metadata listing and read-only snapshot verification.
use std::path::Path;
use serde_json::{Value,json};
pub fn catalog(source:&Path)->Result<Value,String> {
    let parent=source.parent().ok_or("missing database parent")?;
    let daily=parent.join("daily-backups");
    // Check intermediate directory as well as the leaf, rejecting symlinks.
    match std::fs::symlink_metadata(&daily) {
        Ok(meta) if !meta.is_dir()=>return Err("invalid daily directory".into()),
        Err(e) if e.kind()!=std::io::ErrorKind::NotFound=>return Err(e.to_string()),
        _=>{},
    }
    let daily=daily.join(source.file_name().ok_or("missing database filename")?);
    let mut groups=Vec::new();
    for (kind,path) in [("daily",daily),("publication",parent.join("publication-backups"))] {
        let entries=match std::fs::symlink_metadata(&path) {
            Err(e) if e.kind()==std::io::ErrorKind::NotFound=>Vec::new(),
            Err(e)=>return Err(e.to_string()),
            Ok(_)=>homesense_storage::backup::catalog(&path)?,
        };
        let entries:Vec<_>=entries.into_iter().map(|e|json!({"name":e.name,"regular_file":e.regular_file,"bytes":e.bytes.to_string(),"modified_unix_ms":e.modified_unix_ms.map(|v|v.to_string())})).collect();
        groups.push(json!({"kind":kind,"entries":entries}));
    }
    Ok(json!({"verified":false,"groups":groups}))
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Selection {pub kind:String,pub name:String}
impl Selection {
    pub fn validate(&self)->Result<(),String> {
        if !matches!(self.kind.as_str(),"daily"|"publication") || self.name.len()>128 || !self.name.ends_with(".sqlite") || self.name.starts_with('.') || !self.name.bytes().all(|b|b.is_ascii_alphanumeric()||matches!(b,b'-'|b'.'|b'_')) {return Err("invalid backup selection".into());}
        Ok(())
    }
}
/// The private backup directory is owned by this process. Inspection never
/// migrates the file or restores commands, pairings or configuration.
pub fn inspect(source:&Path,context:homesense_core::ExecutionContext,selection:Selection)->Result<Value,String> {
    static INSPECTION:std::sync::Mutex<()>=std::sync::Mutex::new(());
    let _guard=INSPECTION.try_lock().map_err(|_|"backup inspection busy")?;
    selection.validate()?;
    let listing=catalog(source)?;
    let present=listing["groups"].as_array().unwrap().iter().find(|g|g["kind"]==selection.kind).and_then(|g|g["entries"].as_array()).is_some_and(|entries|entries.iter().any(|e|e["name"]==selection.name && e["regular_file"]==true));
    if !present{return Err("backup not available".into());}
    let parent=source.parent().ok_or("database parent missing")?;
    let directory=if selection.kind=="daily" {parent.join("daily-backups").join(source.file_name().ok_or("database filename missing")?)} else {parent.join("publication-backups")};
    let result=homesense_storage::backup::inspect(&directory.join(&selection.name),context)?;
    Ok(json!({"kind":selection.kind,"name":selection.name,"verified":true,"schema":result.schema,"bytes":result.bytes.to_string(),"active_revision":result.active_revision.map(|v|v.to_string()),"configuration_versions":result.configuration_versions.to_string(),"automation_versions":result.automation_versions.to_string()}))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn inspection_validates_selection_and_never_modifies_snapshot() {
        use homesense_core::ExecutionContext;
        for name in ["../x.sqlite","/x.sqlite","x\\y.sqlite","x%2f.sqlite",".sqlite"] {assert!(Selection{kind:"daily".into(),name:name.into()}.validate().is_err());}
        assert!(Selection{kind:"other".into(),name:"x.sqlite".into()}.validate().is_err());
        let root=std::env::temp_dir().join(format!("web-inspection-{}",uuid::Uuid::new_v4()));std::fs::create_dir_all(root.join("publication-backups")).unwrap();
        let source=root.join("source.sqlite");let mut store=homesense_storage::Store::open(&source).unwrap();
        store.publish_automation(None,include_bytes!("../../../config/simulation.json"),br#"{"schema_version":1,"configuration_revision":1,"variables":[],"rules":[]}"#,ExecutionContext::Simulation,&[]).unwrap();
        let path=root.join("publication-backups/test.sqlite");homesense_storage::backup::create(&source,&path,std::time::Duration::from_secs(2)).unwrap();let bytes=std::fs::read(&path).unwrap();
        let selection=||Selection{kind:"publication".into(),name:"test.sqlite".into()};
        let result=inspect(&source,ExecutionContext::Simulation,selection()).unwrap();assert_eq!(result["verified"],true);assert_eq!(result["active_revision"],"1");
        assert!(inspect(&source,ExecutionContext::Live,selection()).is_err());assert_eq!(std::fs::read(path).unwrap(),bytes);
        drop(store);std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn catalog_is_bounded_metadata_without_paths_or_symlink_traversal() {
        let root=std::env::temp_dir().join(format!("web-backups-{}",uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("daily-backups/source.sqlite")).unwrap();
        let source=root.join("source.sqlite");
        std::fs::write(root.join("daily-backups/source.sqlite/daily-1.sqlite"),b"not a database").unwrap();
        let value=catalog(&source).unwrap();
        assert_eq!(value["verified"],false);
        assert_eq!(value["groups"][0]["entries"][0]["bytes"],"14");
        assert!(!value.to_string().contains(root.to_str().unwrap()));
        #[cfg(unix)] {
            std::os::unix::fs::symlink(root.join("daily-backups/source.sqlite"),root.join("publication-backups")).unwrap();
            assert!(catalog(&source).is_err());
            std::fs::remove_file(root.join("publication-backups")).unwrap();
        }
        std::fs::remove_dir_all(root).unwrap();
    }
}
