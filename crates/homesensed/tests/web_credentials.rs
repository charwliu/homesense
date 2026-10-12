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

#[test]
fn bootstrap_never_reinstates_revoked_credentials_or_overwrites_scopes() {
    use homesensed::web_auth::Tokens;
    let root=std::env::temp_dir().join(format!("bootstrap-{}",Uuid::new_v4()));std::fs::create_dir(&root).unwrap();let path=root.join("credentials.sqlite");
    let secret="abcdefghijklmnopqrstuvwxyz0123456789";let header=format!("Bearer {secret}");
    let grant=Grant{controller:Uuid::new_v4(),role:Role::Member,scenes:["one".into()].into(),devices:Default::default()};
    drop(Credentials::open(&path).unwrap());
    let sql=rusqlite::Connection::open(&path).unwrap();
    sql.execute_batch("CREATE TRIGGER fail_initialization BEFORE INSERT ON web_credential_initialized BEGIN SELECT RAISE(ABORT,'injected'); END;").unwrap();
    assert!(Tokens::open_bootstrapped(&path,secret,grant.clone()).is_err());
    assert!(Credentials::open(&path).unwrap().list().unwrap().is_empty());
    sql.execute_batch("DROP TRIGGER fail_initialization").unwrap();drop(sql);
    let first=Tokens::open_bootstrapped(&path,secret,grant.clone()).unwrap();assert_eq!(first.authenticate(&header),Some(grant.clone()));drop(first);
    let mut expanded=grant.clone();expanded.role=Role::Administrator;expanded.scenes.insert("two".into());
    let second=Tokens::open_bootstrapped(&path,secret,expanded.clone()).unwrap();assert_eq!(second.authenticate(&header),Some(grant));
    let id=Credentials::open(&path).unwrap().list().unwrap()[0].id;second.revoke(id).unwrap();drop(second);
    let third=Tokens::open_bootstrapped(&path,secret,expanded).unwrap();assert!(third.authenticate(&header).is_none());drop(third);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn managed_revocation_rechecks_admin_preserves_last_admin_and_invalidates_sessions() {
    use homesensed::web_auth::Tokens;
    let root=std::env::temp_dir().join(format!("managed-revoke-{}",Uuid::new_v4()));std::fs::create_dir(&root).unwrap();let path=root.join("credentials.sqlite");
    let tokens=Tokens::open(&path).unwrap();let admin=Uuid::new_v4();let member=Uuid::new_v4();
    let secret="abcdefghijklmnopqrstuvwxyz0123456789";let member_secret="0123456789abcdefghijklmnopqrstuvwxyz";let header=format!("Bearer {secret}");let member_header=format!("Bearer {member_secret}");
    let grant=Grant{controller:admin,role:Role::Administrator,scenes:Default::default(),devices:Default::default()};
    tokens.insert(admin,secret,grant.clone()).unwrap();let mut member_grant=grant;member_grant.role=Role::Member;tokens.insert(member,member_secret,member_grant).unwrap();
    let session=tokens.login(&header).unwrap();let member_session=tokens.login(&member_header).unwrap();
    assert_eq!(tokens.revoke_managed(member,Some(&member_header),None,None),Err("administrator_required"));
    assert_eq!(tokens.revoke_managed(member,None,Some(&session.cookie),Some("bad")),Err("administrator_required"));
    assert_eq!(tokens.revoke_managed(admin,Some(&header),None,None),Err("last_administrator"));
    let sql=rusqlite::Connection::open(&path).unwrap();sql.execute_batch("CREATE TRIGGER fail_revoke BEFORE DELETE ON web_credential BEGIN SELECT RAISE(ABORT,'failure'); END;").unwrap();
    assert_eq!(tokens.revoke_managed(member,Some(&header),None,None),Err("credential_store_unavailable"));assert!(tokens.session(&member_session.cookie,None,false).is_some());
    sql.execute_batch("DROP TRIGGER fail_revoke").unwrap();
    tokens.revoke_managed(member,None,Some(&session.cookie),Some(&session.csrf)).unwrap();assert!(tokens.session(&member_session.cookie,None,false).is_none());
    assert_eq!(tokens.revoke_managed(member,Some(&header),None,None),Err("credential_not_found"));
    drop(tokens);let tokens=Tokens::open(&path).unwrap();assert!(tokens.authenticate(&member_header).is_none());assert!(tokens.authenticate(&header).is_some());drop(tokens);drop(sql);std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn issuance_is_scoped_persistent_and_never_returns_a_secret_on_storage_failure() {
    use homesensed::web_auth::{Tokens,CredentialRequest};
    let root=std::env::temp_dir().join(format!("issue-{}",Uuid::new_v4()));std::fs::create_dir(&root).unwrap();let path=root.join("credentials.sqlite");
    let tokens=Tokens::open(&path).unwrap();let id=Uuid::new_v4();let secret="abcdefghijklmnopqrstuvwxyz0123456789";let header=format!("Bearer {secret}");
    tokens.insert(id,secret,Grant{controller:id,role:Role::Administrator,devices:["lamp".into()].into(),scenes:["read".into()].into()}).unwrap();
    let input=||CredentialRequest{role:Role::Member,devices:["lamp".into()].into(),scenes:Default::default()};
    let mut expanded=input();expanded.devices.insert("other".into());assert!(matches!(tokens.issue(expanded,Some(&header),None,None),Err("scope_expansion_forbidden")));
    let sql=rusqlite::Connection::open(&path).unwrap();sql.execute_batch("CREATE TRIGGER reject_issue BEFORE INSERT ON web_credential BEGIN SELECT RAISE(ABORT,'failure'); END;").unwrap();
    assert!(matches!(tokens.issue(input(),Some(&header),None,None),Err("credential_store_unavailable")));assert_eq!(tokens.summaries().unwrap().len(),1);
    sql.execute_batch("DROP TRIGGER reject_issue").unwrap();let session=tokens.login(&header).unwrap();
    let issued=tokens.issue(input(),None,Some(&session.cookie),Some(&session.csrf)).unwrap();assert_eq!(issued.secret.len(),64);assert_eq!(issued.grant.controller,id);assert_eq!(issued.grant.role,Role::Member);
    let issued_header=format!("Bearer {}",issued.secret);assert!(matches!(tokens.issue(input(),Some(&issued_header),None,None),Err("administrator_required")));
    let stored:String=sql.query_row("SELECT grant_json FROM web_credential WHERE id=?1",[issued.id.to_string()],|row|row.get(0)).unwrap();assert!(!stored.contains(&issued.secret));
    drop(tokens);let tokens=Tokens::open(&path).unwrap();assert_eq!(tokens.authenticate(&issued_header),Some(issued.grant));assert_eq!(tokens.summaries().unwrap().len(),2);
    drop(tokens);drop(sql);std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn grant_updates_are_atomic_scoped_and_force_reauthentication() {
    use homesensed::web_auth::{Tokens,CredentialRequest,CredentialUpdate};
    let root=std::env::temp_dir().join(format!("grant-update-{}",Uuid::new_v4()));std::fs::create_dir(&root).unwrap();let path=root.join("credentials.sqlite");
    let tokens=Tokens::open(&path).unwrap();let admin=Uuid::new_v4();let secret="abcdefghijklmnopqrstuvwxyz0123456789";let header=format!("Bearer {secret}");
    tokens.insert(admin,secret,Grant{controller:admin,role:Role::Administrator,devices:["lamp".into()].into(),scenes:Default::default()}).unwrap();
    let admin_grant=tokens.authenticate(&header).unwrap();
    let input=|expected:Grant|CredentialUpdate{expected,role:Role::Viewer,devices:Default::default(),scenes:Default::default()};
    assert!(matches!(tokens.update_grant(admin,input(admin_grant),Some(&header),None,None),Err("last_administrator")));
    let issued=tokens.issue(CredentialRequest{role:Role::Member,devices:["lamp".into()].into(),scenes:Default::default()},Some(&header),None,None).unwrap();let user_header=format!("Bearer {}",issued.secret);let session=tokens.login(&user_header).unwrap();
    let mut expanded=input(issued.grant.clone());expanded.devices.insert("outside".into());assert!(matches!(tokens.update_grant(issued.id,expanded,Some(&header),None,None),Err("scope_expansion_forbidden")));
    assert!(matches!(tokens.update_grant(issued.id,input(issued.grant.clone()),Some(&user_header),None,None),Err("administrator_required")));
    let sql=rusqlite::Connection::open(&path).unwrap();sql.execute_batch("CREATE TRIGGER reject_grant BEFORE UPDATE ON web_credential BEGIN SELECT RAISE(ABORT,'failure'); END;").unwrap();
    assert!(matches!(tokens.update_grant(issued.id,input(issued.grant.clone()),Some(&header),None,None),Err("credential_store_unavailable")));assert_eq!(tokens.session(&session.cookie,None,false).unwrap().role,Role::Member);
    sql.execute_batch("DROP TRIGGER reject_grant").unwrap();tokens.update_grant(issued.id,input(issued.grant.clone()),Some(&header),None,None).unwrap();assert!(tokens.session(&session.cookie,None,false).is_none());
    assert!(matches!(tokens.update_grant(issued.id,input(issued.grant.clone()),Some(&header),None,None),Err("credential_changed")));
    let new_grant=tokens.authenticate(&user_header).unwrap();assert_eq!(new_grant.role,Role::Viewer);assert!(new_grant.devices.is_empty());assert_eq!(new_grant.controller,admin);
    drop(tokens);let tokens=Tokens::open(&path).unwrap();assert_eq!(tokens.authenticate(&user_header),Some(new_grant));drop(tokens);drop(sql);std::fs::remove_dir_all(root).unwrap();
}
