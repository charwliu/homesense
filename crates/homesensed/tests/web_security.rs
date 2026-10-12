use homesensed::{
    web_auth::{Grant, Role, Tokens},
    web_receipts::{Claim, Receipts},
};
use uuid::Uuid;
#[test]
fn token_scope_role_and_revocation_are_independent_of_runtime_identity() {
    let tokens = Tokens::default();
    let id = Uuid::new_v4();
    let caller = Uuid::new_v4();
    let secret = "abcdefghijklmnopqrstuvwxyz0123456789";
    tokens
        .insert(
            id,
            secret,
            Grant {
                controller: caller,
                role: Role::Viewer,
                scenes: ["reading".into()].into(),
                devices: ["lamp".into()].into(),
            },
        )
        .unwrap();
    assert!(tokens.authenticate(secret).is_none());
    assert!(
        tokens
            .authenticate("Bearer abcdefghijklmnopqrstuvwxyz0123456780")
            .is_none()
    );
    let grant = tokens.authenticate(&format!("Bearer {secret}")).unwrap();
    assert_eq!(grant.controller, caller);
    assert!(grant.may_read("reading"));
    assert!(!grant.may_control("reading"));
    assert!(!grant.may_read("other"));
    assert!(grant.may_read_device("lamp"));
    assert!(!grant.may_control_device("lamp"));
    assert!(!grant.may_read_device("other"));
    assert!(
        tokens
            .insert(
                Uuid::new_v4(),
                secret,
                Grant {
                    controller: caller,
                    role: Role::Administrator,
                    scenes: ["other".into()].into(),
                    devices: Default::default()
                }
            )
            .is_err()
    );
    tokens.revoke(id).unwrap();
    assert!(tokens.authenticate(&format!("Bearer {secret}")).is_none());
}
#[test]
fn durable_fence_never_replays_unknown_or_completed_requests_after_restart() {
    let path = std::env::temp_dir().join(format!("web-receipts-{}.sqlite", Uuid::new_v4()));
    let mut receipts = Receipts::open(&path).unwrap();
    assert_eq!(
        receipts.claim("caller", "key", "scene:v1").unwrap(),
        Claim::New
    );
    drop(receipts);
    let mut receipts = Receipts::open(&path).unwrap();
    assert_eq!(
        receipts.claim("caller", "key", "scene:v1").unwrap(),
        Claim::Existing(None)
    );
    assert_eq!(
        receipts.claim("caller", "key", "different").unwrap(),
        Claim::Conflict
    );
    receipts.finish("caller", "key", "accepted").unwrap();
    assert!(receipts.finish("caller", "key", "changed").is_err());
    drop(receipts);
    let mut receipts = Receipts::open(&path).unwrap();
    assert_eq!(
        receipts.claim("caller", "key", "scene:v1").unwrap(),
        Claim::Existing(Some("accepted".into()))
    );
    assert_eq!(
        receipts.claim("other", "key", "scene:v1").unwrap(),
        Claim::New
    );
    drop(receipts);
    std::fs::remove_file(path).unwrap();
}
#[test]
fn browser_sessions_require_csrf_and_follow_token_revocation() {
    let tokens = Tokens::default();
    let id = Uuid::new_v4();
    let header = "Bearer abcdefghijklmnopqrstuvwxyz0123456789";
    tokens
        .insert(
            id,
            &header[7..],
            Grant {
                controller: id,
                role: Role::Member,
                scenes: ["scene".into()].into(),
                devices: Default::default(),
            },
        )
        .unwrap();
    let session = tokens.login(header).unwrap();
    assert!(tokens.session(&session.cookie, None, false).is_some());
    assert!(tokens.session(&session.cookie, None, true).is_none());
    assert!(
        tokens
            .session(&session.cookie, Some("wrong"), true)
            .is_none()
    );
    assert!(
        tokens
            .session(&session.cookie, Some(&session.csrf), true)
            .is_some()
    );
    tokens.logout(&session.cookie);
    assert!(tokens.session(&session.cookie, None, false).is_none());
    let session = tokens.login(header).unwrap();
    tokens.revoke(id).unwrap();
    assert!(
        tokens
            .session(&session.cookie, Some(&session.csrf), true)
            .is_none()
    );
}

#[test]
fn reissued_credentials_never_resurrect_old_browser_sessions() {
    let tokens=Tokens::default();let id=Uuid::new_v4();
    let secret="abcdefghijklmnopqrstuvwxyz0123456789";let header=format!("Bearer {secret}");
    let grant=Grant{controller:id,role:Role::Member,scenes:["scene".into()].into(),devices:Default::default()};
    tokens.insert(id,secret,grant.clone()).unwrap();
    let original=tokens.login(&header).unwrap();
    assert!(tokens.session(&original.cookie,Some(&original.csrf),true).is_some());
    tokens.revoke(id).unwrap();tokens.insert(id,secret,grant.clone()).unwrap();
    assert!(tokens.session(&original.cookie,None,false).is_none());
    assert!(tokens.session(&original.cookie,Some(&original.csrf),true).is_none());
    let renewed=tokens.login(&header).unwrap();
    let mut elevated=grant.clone();elevated.role=Role::Administrator;
    tokens.insert(id,secret,elevated).unwrap();
    assert!(tokens.session(&renewed.cookie,None,false).is_none());
    assert_eq!(tokens.session(&tokens.login(&header).unwrap().cookie,None,false).unwrap().role,Role::Administrator);
    tokens.revoke(id).unwrap();tokens.insert(Uuid::new_v4(),secret,grant).unwrap();
    assert!(tokens.session(&original.cookie,None,false).is_none());
    assert!(tokens.session(&renewed.cookie,None,false).is_none());
}
