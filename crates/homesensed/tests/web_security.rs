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
    assert!(tokens.insert(Uuid::new_v4(),secret,Grant{controller:caller,role:Role::Administrator,scenes:["other".into()].into()}).is_err());
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
