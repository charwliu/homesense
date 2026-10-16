//! Deterministic source credential changes between admission and dispatch.
//! In-memory streams only; another credential keeps the same controller alive.
use crate::{
    admission::{Admission, Options},
    management_keypads::{self, Setting},
    panel_auth::Lease,
    transport_owner::{TransportOwner, send_ticket},
    web_auth::{
        CredentialRequest, CredentialUpdate, Grant, IssuedCredential, ModuleOffLease, Role,
        SessionSecret, Tokens,
    },
};
use homesense_core::{Command, DeviceId, ExecutionContext};
use homesense_storage::{Store, journal::Status};
use std::{
    collections::BTreeSet,
    io::{self, Read, Write},
    path::PathBuf,
    sync::{Arc, Mutex},
};
use uuid::Uuid;
const ROOT: &str = "synthetic-panel-root-credential-0123456789";
struct Wire(Arc<Mutex<Vec<u8>>>);
impl Read for Wire {
    fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
        Err(io::ErrorKind::WouldBlock.into())
    }
}
impl Write for Wire {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
struct Fixture {
    path: PathBuf,
    actor: Admission,
    tokens: Arc<Tokens>,
    source: IssuedCredential,
    session: Option<SessionSecret>,
    lease: ModuleOffLease,
    config: homesense_config::Configuration,
    bytes: Arc<Mutex<Vec<u8>>>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", self.path.display()));
        }
        let _ = std::fs::remove_file(self.path.with_extension("runtime.lock"));
    }
}
impl Fixture {
    async fn new(cookie: bool) -> Self {
        let keys=(1..=6).map(|n|serde_json::json!({"number":n,"label":n.to_string(),"led_endpoint":{"id":format!("key:{n}"),"name":format!("LED {n}"),"enabled":true}})).collect::<Vec<_>>();
        let definition=serde_json::to_vec(&serde_json::json!({"schema_version":2,"revision":1,"gateways":[{"id":"bus","endpoint":"127.0.0.1:1","modules":[{"id":"module","address":"41","profile":"ts6_86_rs485_v2_6_5_6key","verification":{"status":"simulated"},"led_mode":"manual"}]}],"devices":[],"keypads":[{"id":"panel","name":"Panel","module_id":"module","enabled":true,"keys":keys}]})).unwrap();
        let path = std::env::temp_dir().join(format!("panel-auth-{}.sqlite", Uuid::new_v4()));
        Store::open(&path)
            .unwrap()
            .publish_configuration(None, &definition, ExecutionContext::Simulation, &[])
            .unwrap();
        let config =
            homesense_config::Configuration::parse(&definition, ExecutionContext::Simulation)
                .unwrap();
        let controller = Uuid::new_v4();
        let tokens = Arc::new(Tokens::default());
        let devices = (1..=6).map(|n| format!("key:{n}")).collect::<BTreeSet<_>>();
        tokens
            .insert(
                Uuid::new_v4(),
                ROOT,
                Grant {
                    controller,
                    role: Role::Administrator,
                    devices: devices.clone(),
                    scenes: BTreeSet::new(),
                },
            )
            .unwrap();
        let source = tokens
            .issue(
                CredentialRequest {
                    role: Role::Administrator,
                    devices,
                    scenes: BTreeSet::new(),
                },
                Some(&format!("Bearer {ROOT}")),
                None,
                None,
            )
            .unwrap();
        assert_eq!(source.grant.controller, controller);
        let session = cookie.then(|| tokens.login(&format!("Bearer {}", source.secret)).unwrap());
        let lease = if let Some(session) = &session {
            tokens.module_off_lease(None, Some(&session.cookie), Some(&session.csrf))
        } else {
            tokens.module_off_lease(Some(&format!("Bearer {}", source.secret)), None, None)
        }
        .unwrap();
        let owner = tokens.clone();
        let actor = Admission::open(
            path.clone(),
            Options {
                context: ExecutionContext::Simulation,
                ..Options::default()
            },
            move |id| {
                owner
                    .authenticate(&format!("Bearer {ROOT}"))
                    .is_some_and(|g| g.controller == id)
            },
        )
        .await
        .unwrap();
        actor.synchronized(1, 1).await.unwrap();
        Self {
            path,
            actor,
            tokens,
            source,
            session,
            lease,
            config,
            bytes: Arc::new(Mutex::new(vec![])),
        }
    }
    fn change(&self, kind: &str) {
        let auth = format!("Bearer {ROOT}");
        match kind {
            "revoke" => self
                .tokens
                .revoke_managed(self.source.id, Some(&auth), None, None)
                .unwrap(),
            "logout" => self.tokens.logout(&self.session.as_ref().unwrap().cookie),
            _ => {
                let mut devices = self.source.grant.devices.clone();
                if kind == "scope" {
                    devices = ["key:1".into()].into();
                }
                self.tokens
                    .update_grant(
                        self.source.id,
                        CredentialUpdate {
                            expected: self.source.grant.clone(),
                            role: if kind == "role" {
                                Role::Member
                            } else {
                                Role::Administrator
                            },
                            devices,
                            scenes: BTreeSet::new(),
                        },
                        Some(&auth),
                        None,
                        None,
                    )
                    .unwrap();
            }
        }
        assert!(
            self.tokens.authenticate(&auth).is_some(),
            "another credential still authorizes the same controller"
        );
        assert!(!self.lease.valid());
    }
    async fn prepared(&self, led: bool) -> (DeviceId, Command, Lease) {
        if led {
            let device = DeviceId::new("key:1").unwrap();
            let command = Command::PanelLed(true);
            let lease =
                Lease::led(self.lease.clone(), &self.config, device.clone(), command).unwrap();
            (device, command, lease)
        } else {
            let (batch, lease) = management_keypads::prepare(
                &self.actor,
                self.source.grant.controller,
                self.actor.task_generation().await.unwrap(),
                "panel",
                self.lease.clone(),
                Setting::Sound { enabled: false },
            )
            .await
            .unwrap();
            let intent = batch.intents.into_iter().next().unwrap();
            (intent.device, intent.command, lease)
        }
    }
    fn owner(&self) -> TransportOwner<Wire> {
        TransportOwner::new(
            self.actor.clone(),
            [(
                "bus".into(),
                hb_transport::Connection::for_simulation(Wire(self.bytes.clone()), 1),
            )]
            .into(),
        )
    }
}
#[tokio::test]
async fn source_generation_changes_before_enqueue_and_dispatch_cancel_parameters_and_leds() {
    for led in [false, true] {
        for queued in [false, true] {
            for kind in ["role", "scope", "revoke", "logout"] {
                let fixture = Fixture::new(kind == "logout").await;
                let (device, command, lease) = fixture.prepared(led).await;
                let accepted = if queued {
                    Some(
                        fixture
                            .actor
                            .submit_panel_command_for_revision(
                                1,
                                fixture.source.grant.controller,
                                device.clone(),
                                command,
                                lease.clone(),
                            )
                            .await
                            .unwrap(),
                    )
                } else {
                    None
                };
                fixture.change(kind);
                if !queued {
                    assert!(
                        fixture
                            .actor
                            .submit_panel_command_for_revision(
                                1,
                                fixture.source.grant.controller,
                                device,
                                command,
                                lease
                            )
                            .await
                            .is_err()
                    );
                    let sql = rusqlite::Connection::open(&fixture.path).unwrap();
                    assert_eq!(
                        sql.query_row("SELECT count(*) FROM command_journal", [], |r| r
                            .get::<_, i64>(0))
                            .unwrap(),
                        0
                    );
                }
                let mut owner = fixture.owner();
                assert!(owner.step().await.unwrap().is_none());
                assert!(
                    fixture.bytes.lock().unwrap().is_empty(),
                    "{kind}, LED={led}, queued={queued}"
                );
                if let Some(ids) = accepted {
                    assert_eq!(
                        Store::open(&fixture.path)
                            .unwrap()
                            .command_status(fixture.actor.boot_id(), ids[0])
                            .unwrap(),
                        Some(Status::Cancelled)
                    );
                }
                // A fresh, independent root source still writes through this same owner;
                // zero bytes above did not come from a broken transport or dead controller.
                let raw = fixture
                    .tokens
                    .module_off_lease(Some(&format!("Bearer {ROOT}")), None, None)
                    .unwrap();
                let (batch, lease) = management_keypads::prepare(
                    &fixture.actor,
                    fixture.source.grant.controller,
                    fixture.actor.task_generation().await.unwrap(),
                    "panel",
                    raw,
                    Setting::Sound { enabled: false },
                )
                .await
                .unwrap();
                let intent = batch.intents.into_iter().next().unwrap();
                fixture
                    .actor
                    .submit_panel_command_for_revision(
                        1,
                        fixture.source.grant.controller,
                        intent.device,
                        intent.command,
                        lease,
                    )
                    .await
                    .unwrap();
                owner.step().await.unwrap().unwrap().result.unwrap();
                assert_eq!(*fixture.bytes.lock().unwrap(), [0xAE, 0x41, 0xC2, 2, 0xEE]);
                fixture.actor.clone().shutdown().await.unwrap();
            }
        }
    }
}
#[tokio::test]
async fn source_revocation_after_dispatch_ticket_still_prevents_the_actual_write() {
    for led in [false, true] {
        let fixture = Fixture::new(false).await;
        let (device, command, lease) = fixture.prepared(led).await;
        let ids = fixture
            .actor
            .submit_panel_command_for_revision(
                1,
                fixture.source.grant.controller,
                device,
                command,
                lease,
            )
            .await
            .unwrap();
        let ticket = fixture.actor.dispatch().await.unwrap().unwrap();
        fixture.change("revoke");
        let mut connection =
            hb_transport::Connection::for_simulation(Wire(fixture.bytes.clone()), 1);
        let result =
            send_ticket(&ticket, &mut connection, || fixture.actor.monotonic_ms()).unwrap_err();
        assert_eq!(result.bytes_written, 0);
        assert!(fixture.bytes.lock().unwrap().is_empty());
        assert_eq!(
            result.reason,
            hb_transport::SendFailure::Io(io::ErrorKind::PermissionDenied)
        );
        fixture.actor.finish_write(ids[0], false).await.unwrap();
        assert_ne!(
            Store::open(&fixture.path)
                .unwrap()
                .command_status(fixture.actor.boot_id(), ids[0])
                .unwrap(),
            Some(Status::Written)
        );
        fixture.actor.clone().shutdown().await.unwrap();
    }
}
struct PartialSetting {bytes:Arc<Mutex<Vec<u8>>>,fail_after:usize}
impl Read for PartialSetting{fn read(&mut self,_:&mut[u8])->io::Result<usize>{Err(io::ErrorKind::WouldBlock.into())}}
impl Write for PartialSetting{fn write(&mut self,b:&[u8])->io::Result<usize>{let mut bytes=self.bytes.lock().unwrap();if bytes.len()>=self.fail_after{return Err(io::ErrorKind::BrokenPipe.into())}let n=b.len().min(self.fail_after-bytes.len());bytes.extend_from_slice(&b[..n]);Ok(n)}fn flush(&mut self)->io::Result<()>{Ok(())}}
#[tokio::test]
async fn ts6_setting_triplet_is_one_receipt_and_only_complete_operation_is_written(){
    for (setting,command) in [(Setting::Backlight{mode:management_keypads::Backlight::Bright},0xc6),(Setting::WorkMode{mode:management_keypads::WorkMode::Manual},0xc8)]{for fail_after in [5,10,15]{let f=Fixture::new(false).await;let (batch,lease)=management_keypads::prepare(&f.actor,f.source.grant.controller,f.actor.task_generation().await.unwrap(),"panel",f.lease.clone(),setting.clone()).await.unwrap();let intent=batch.intents.into_iter().next().unwrap();let ids=f.actor.submit_panel_command_for_revision(1,f.source.grant.controller,intent.device,intent.command,lease).await.unwrap();assert_eq!(ids.len(),1,"three frames share one logical ticket");let mut owner=TransportOwner::new(f.actor.clone(),[("bus".into(),hb_transport::Connection::for_simulation(PartialSetting{bytes:f.bytes.clone(),fail_after},1))].into());let result=owner.step().await.unwrap().unwrap().result;assert_eq!(result.is_ok(),fail_after==15);assert_eq!(f.bytes.lock().unwrap().len(),fail_after);assert_eq!(*f.bytes.lock().unwrap(),[0xae,0x41,command,2,0xee].repeat(fail_after/5));let store=Store::open(&f.path).unwrap();assert_eq!(store.command_status(f.actor.boot_id(),ids[0]).unwrap(),Some(if fail_after==15{Status::Written}else{Status::Uncertain}));drop(store);if fail_after<15{assert!(owner.step().await.is_err());assert_eq!(f.bytes.lock().unwrap().len(),fail_after,"failed partial setting never replays");}drop(owner);f.actor.clone().shutdown().await.unwrap();}}
}
#[tokio::test]
async fn ts6_setting_revoked_before_dispatch_cancels_whole_triplet_without_wire_bytes(){
    let f=Fixture::new(false).await;let (batch,lease)=management_keypads::prepare(&f.actor,f.source.grant.controller,f.actor.task_generation().await.unwrap(),"panel",f.lease.clone(),Setting::Backlight{mode:management_keypads::Backlight::Bright}).await.unwrap();let intent=batch.intents.into_iter().next().unwrap();let ids=f.actor.submit_panel_command_for_revision(1,f.source.grant.controller,intent.device,intent.command,lease).await.unwrap();f.change("revoke");let mut owner=f.owner();assert!(owner.step().await.unwrap().is_none());assert!(f.bytes.lock().unwrap().is_empty());assert_eq!(Store::open(&f.path).unwrap().command_status(f.actor.boot_id(),ids[0]).unwrap(),Some(Status::Cancelled));drop(owner);f.actor.clone().shutdown().await.unwrap();
}
