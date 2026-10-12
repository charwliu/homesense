use homesensed::{web_auth::{Grant,Role},web_credentials::{Credentials,Record}};
use uuid::Uuid;
#[test]
fn persistent_credentials_are_private_atomic_and_revocable() {
    let root=std::env::temp_dir().join(format!("credentials-{}",Uuid::new_v4()));std::fs::create_dir(&root).unwrap();let path=root.join("credentials.sqlite");
    let id=Uuid::new_v4();let grant=Grant{controller:Uuid::new_v4(),role:Role::Member,scenes:["scene".into()].into(),devices:["lamp".into()].into()};
    let mut store=Credentials::open(&path).unwrap();store.save(&Record{id,digest:[1;32],grant:grant.clone()}).unwrap();drop(store);
    let mut store=Credentials::open(&path).unwrap();let records=store.list().unwrap();assert_eq!(records.len(),1);assert_eq!(records[0].grant,grant);
    assert!(store.save(&Record{id:Uuid::new_v4(),digest:[1;32],grant:grant.clone()}).is_err());assert_eq!(store.list().unwrap().len(),1);
    let sql=rusqlite::Connection::open(&path).unwrap();sql.execute_batch("CREATE TRIGGER fail_replace BEFORE UPDATE ON web_credential BEGIN SELECT RAISE(ABORT,'synthetic failure'); END;").unwrap();
    let mut changed=grant.clone();changed.role=Role::Administrator;
    assert!(store.save(&Record{id,digest:[2;32],grant:changed.clone()}).is_err());assert_eq!(store.list().unwrap()[0].grant,grant);
    sql.execute_batch("DROP TRIGGER fail_replace").unwrap();store.save(&Record{id,digest:[2;32],grant:changed.clone()}).unwrap();drop(store);
    let mut store=Credentials::open(&path).unwrap();assert_eq!(store.list().unwrap()[0].grant,changed);assert_eq!(store.list().unwrap()[0].digest,[2;32]);
    store.revoke(id).unwrap();drop(store);assert!(Credentials::open(&path).unwrap().list().unwrap().is_empty());
    sql.execute("INSERT INTO web_credential VALUES(?1,?2,'{invalid}')",rusqlite::params![id.to_string(),[3u8;32].as_slice()]).unwrap();assert!(Credentials::open(&path).is_err());sql.execute("DELETE FROM web_credential",[]).unwrap();assert!(Credentials::open(&path).is_ok());drop(sql);
    #[cfg(unix)] {
        use std::os::unix::fs::{PermissionsExt,symlink};
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode()&0o077,0);
        let link=root.join("link.sqlite");symlink(&path,&link).unwrap();assert!(Credentials::open(&link).is_err());
        std::fs::set_permissions(&path,std::fs::Permissions::from_mode(0o644)).unwrap();assert!(Credentials::open(&path).is_err());
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn persistent_tokens_commit_before_memory_and_restart_without_sessions() {
    use homesensed::web_auth::Tokens;
    let root=std::env::temp_dir().join(format!("persistent-tokens-{}",Uuid::new_v4()));std::fs::create_dir(&root).unwrap();let path=root.join("credentials.sqlite");
    let id=Uuid::new_v4();let secret="abcdefghijklmnopqrstuvwxyz0123456789";let header=format!("Bearer {secret}");
    let grant=Grant{controller:id,role:Role::Member,scenes:["scene".into()].into(),devices:Default::default()};
    let tokens=Tokens::open(&path).unwrap();tokens.insert(id,secret,grant.clone()).unwrap();let session=tokens.login(&header).unwrap();
    drop(tokens);let tokens=Tokens::open(&path).unwrap();assert_eq!(tokens.authenticate(&header),Some(grant.clone()));assert!(tokens.session(&session.cookie,None,false).is_none());
    let current=tokens.login(&header).unwrap();let sql=rusqlite::Connection::open(&path).unwrap();
    sql.execute_batch("CREATE TRIGGER reject_update BEFORE UPDATE ON web_credential BEGIN SELECT RAISE(ABORT,'failure'); END; CREATE TRIGGER reject_delete BEFORE DELETE ON web_credential BEGIN SELECT RAISE(ABORT,'failure'); END;").unwrap();
    let mut elevated=grant.clone();elevated.role=Role::Administrator;
    assert!(tokens.insert(id,secret,elevated.clone()).is_err());assert_eq!(tokens.authenticate(&header),Some(grant.clone()));assert!(tokens.session(&current.cookie,None,false).is_some());
    assert!(tokens.revoke(id).is_err());assert_eq!(tokens.authenticate(&header),Some(grant));
    sql.execute_batch("DROP TRIGGER reject_update; DROP TRIGGER reject_delete;").unwrap();tokens.insert(id,secret,elevated.clone()).unwrap();assert!(tokens.session(&current.cookie,None,false).is_none());
    drop(tokens);let tokens=Tokens::open(&path).unwrap();assert_eq!(tokens.authenticate(&header),Some(elevated));tokens.revoke(id).unwrap();drop(tokens);
    let tokens=Tokens::open(&path).unwrap();assert!(tokens.authenticate(&header).is_none());drop(tokens);drop(sql);std::fs::remove_dir_all(root).unwrap();
}
