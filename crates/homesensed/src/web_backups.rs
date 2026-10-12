//! Metadata only; filenames never become client-selected filesystem paths.
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

#[cfg(test)]
mod tests {
    use super::*;
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
