use super::*;
use crate::{admission::Options, transport_owner::GatewayRead};
use homesense_core::{Command, ExecutionContext, LevelTransition, ModuleKind};
use homesense_storage::Store;
use serde_json::json;
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

const IDS: [&str; 6] = ["common", "left", "right", "primary", "secondary", "reading"];
fn id(value: &str) -> DeviceId {
    DeviceId::new(value).unwrap()
}
fn p(value: u8) -> Percent {
    Percent::new(value).unwrap()
}
fn document() -> Vec<u8> {
    let devices=IDS.iter().enumerate().map(|(index,id)| {
        let mut value=json!({"id":id,"name":id,"module_id":if index<3{"relay"}else{"dimmer"},"channel":index%3+1,"enabled":true});
        if index>=3 {value["default_on_level"]=json!(100);}
        value
    }).collect::<Vec<_>>();
    serde_json::to_vec(&json!({"schema_version":2,"revision":1,"gateways":[{"id":"bus","endpoint":"127.0.0.1:1","modules":[
        {"id":"relay","address":"31","profile":"r8i8_din_pro_v2_7_7","verification":{"status":"simulated"}},
        {"id":"dimmer","address":"32","profile":"ad8_din_rs485_v2_2_1","verification":{"status":"simulated"}}
    ]}],"devices":devices,"shared_lights":[{"id":"pair","common_supply":"common","left_supply":"left","right_supply":"right","primary":"primary","secondary":"secondary","reading":"reading","review_evidence":"synthetic matrix and explicit preservation behavior"}]})).unwrap()
}

struct Fixture {
    root: PathBuf,
    admission: Admission,
    configuration: Configuration,
    feedback: Feedback,
    controller: Uuid,
    generation: TaskGeneration,
    time: &'static AtomicU64,
}
impl Fixture {
    async fn new(values: [u8; 6]) -> Self {
        let root = std::env::temp_dir().join(format!("shared-light-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let bytes = document();
        let path = root.join("runtime.sqlite");
        let mut store = Store::open(&path).unwrap();
        store
            .publish_configuration(None, &bytes, ExecutionContext::Simulation, &[])
            .unwrap();
        store
            .remember_light_levels(1, &[(id("primary"), p(35)), (id("secondary"), p(45))])
            .unwrap();
        drop(store);
        let time = Box::leak(Box::new(AtomicU64::new(0)));
        let controller = Uuid::new_v4();
        let admission = Admission::open_with_test_time(
            path,
            Options {
                context: ExecutionContext::Simulation,
                ..Default::default()
            },
            move |value| value == controller,
            time,
        )
        .await
        .unwrap();
        admission.synchronized(1, 1).await.unwrap();
        let generation = admission.task_generation().await.unwrap();
        let configuration = Configuration::parse(&bytes, ExecutionContext::Simulation).unwrap();
        let feedback = Feedback::new(
            &configuration,
            ExecutionContext::Simulation,
            Uuid::parse_str(admission.boot_id()).unwrap().as_u128(),
            1,
        )
        .unwrap();
        let mut f = Self {
            root,
            admission,
            configuration,
            feedback,
            controller,
            generation,
            time,
        };
        f.sample(values, 0);
        f
    }
    fn sample(&mut self, values: [u8; 6], at: u64) {
        let frames = values
            .into_iter()
            .enumerate()
            .map(|(index, value)| hb_protocol::RawFrame {
                address: if index < 3 { 0x31 } else { 0x32 },
                command: 0xD1 + (index % 3) as u8,
                value: if index < 3 {
                    if value != 0 { 0xF1 } else { 0xF2 }
                } else {
                    value
                },
            })
            .collect();
        self.feedback
            .apply(&GatewayRead {
                gateway_id: "bus".into(),
                transport_epoch: 1,
                received_at_ms: at,
                result: hb_transport::Receive::Frames {
                    transport_epoch: 1,
                    frames,
                },
            })
            .unwrap();
    }
    async fn prepare(&self, target: &str, action: DeviceAction) -> Result<Prepared, String> {
        prepare(
            &self.admission,
            self.controller,
            self.generation.clone(),
            &id(target),
            &action,
            &self.feedback,
            5000,
        )
        .await?
        .ok_or("group missing".into())
    }
    async fn drain(&self) -> Vec<(String, Command)> {
        let mut writes = vec![];
        while let Some(ticket) = self.admission.dispatch().await.unwrap() {
            writes.push((
                ticket.command.device_id.as_str().into(),
                ticket.command.command,
            ));
            self.admission
                .finish_write(ticket.command.id, true)
                .await
                .unwrap();
            self.time.fetch_add(20, Ordering::Release);
        }
        writes
    }
    async fn close(self) {
        self.admission.shutdown().await.unwrap();
        std::fs::remove_dir_all(self.root).unwrap();
    }
}

#[tokio::test]
async fn actual_admission_preserves_reading_for_secondary_on_and_off() {
    for (values, target, expected_common) in [
        ([0, 1, 0, 0, 0, 70], 100, true),
        ([1, 0, 1, 0, 100, 40], 0, false),
    ] {
        let f = Fixture::new(values).await;
        let prepared = f
            .prepare("secondary", DeviceAction::Brightness { level: target })
            .await
            .unwrap();
        let ids = f
            .admission
            .submit_shared_light(f.generation.clone(), prepared, 1000)
            .await
            .unwrap();
        let writes = f.drain().await;
        assert_eq!(ids.len(), if target == 0 { 2 } else { 3 });
        assert_eq!(writes.len(), if target == 0 { 2 } else { 3 });
        assert!(
            !writes
                .iter()
                .any(|(id, _)| ["left", "right", "reading"].contains(&id.as_str()))
        );
        assert!(writes.contains(&(
            "secondary".into(),
            Command::SetLevel {
                level: p(target),
                transition: LevelTransition::Immediate
            }
        )));
        assert!(writes.contains(&("common".into(), Command::Switch(expected_common))));
        f.close().await;
    }
}

#[tokio::test]
async fn explicit_secondary_off_does_not_write_the_active_night_resources() {
    let f = Fixture::new([0, 1, 1, 10, 0, 0]).await;
    let prepared = f
        .prepare("secondary", DeviceAction::Power { on: false })
        .await
        .unwrap();
    f.admission
        .submit_shared_light(f.generation.clone(), prepared, 1000)
        .await
        .unwrap();
    assert_eq!(
        f.drain().await,
        vec![(
            "secondary".into(),
            Command::SetLevel {
                level: p(0),
                transition: LevelTransition::Immediate,
            }
        )]
    );
    f.close().await;
}

#[tokio::test]
async fn night_to_secondary_zeros_latent_primary_and_exits_supplies_before_writing_level() {
    let f = Fixture::new([0, 1, 1, 10, 0, 0]).await;
    let prepared = f
        .prepare("secondary", DeviceAction::Power { on: true })
        .await
        .unwrap();
    f.admission
        .submit_shared_light(f.generation.clone(), prepared, 1000)
        .await
        .unwrap();
    let writes = f.drain().await;
    assert_eq!(
        &writes[..2],
        &[
            ("left".into(), Command::Switch(false)),
            ("right".into(), Command::Switch(false))
        ]
    );
    assert_eq!(
        writes[2],
        (
            "primary".into(),
            Command::SetLevel {
                level: p(0),
                transition: LevelTransition::Immediate
            }
        )
    );
    assert_eq!(
        writes[3],
        (
            "secondary".into(),
            Command::SetLevel {
                level: p(45),
                transition: LevelTransition::Immediate
            }
        )
    );
    assert_eq!(writes[4], ("common".into(), Command::Switch(true)));
    f.close().await;
}

#[tokio::test]
async fn shared_off_does_not_cut_the_lit_peer_and_main_restore_ignores_bedside_level() {
    for (values, action) in [
        ([1, 0, 0, 35, 45, 0], DeviceAction::Power { on: false }),
        ([0, 1, 0, 10, 0, 0], DeviceAction::Power { on: true }),
    ] {
        let f = Fixture::new(values).await;
        let prepared = f.prepare("primary", action).await.unwrap();
        f.admission
            .submit_shared_light(f.generation.clone(), prepared, 1000)
            .await
            .unwrap();
        let writes = f.drain().await;
        assert!(!writes.contains(&("common".into(), Command::Switch(false))));
        assert!(writes.contains(&(
            "primary".into(),
            Command::SetLevel {
                level: p(if values[0] != 0 { 0 } else { 35 }),
                transition: LevelTransition::Immediate
            }
        )));
        f.close().await;
    }
}

#[tokio::test]
async fn changed_or_expired_feedback_rejects_entire_plan_without_queue_mutation() {
    for expired in [false, true] {
        let mut f = Fixture::new([0, 1, 0, 0, 0, 70]).await;
        let prepared = f
            .prepare("secondary", DeviceAction::Power { on: true })
            .await
            .unwrap();
        if expired {
            f.time.store(6000, Ordering::Release)
        } else {
            f.sample([0, 0, 0, 0, 0, 70], 0)
        }
        assert!(
            f.admission
                .submit_shared_light(f.generation.clone(), prepared, 7000)
                .await
                .is_err()
        );
        assert!(f.admission.dispatch().await.unwrap().is_none());
        f.close().await;
    }
}

#[tokio::test]
async fn bare_manual_pair_intent_cannot_bypass_required_physical_lease() {
    let f = Fixture::new([1, 0, 0, 35, 45, 0]).await;
    assert!(
        f.admission
            .submit_for_revision(
                1,
                Batch {
                    controller: f.controller,
                    subscriptions: vec![],
                    intents: vec![Intent {
                        device: id("primary"),
                        command: Command::SetLevel {
                            level: p(0),
                            transition: LevelTransition::Immediate
                        }
                    }]
                }
            )
            .await
            .is_err()
    );
    assert!(f.admission.dispatch().await.unwrap().is_none());
    f.close().await;
}

#[tokio::test]
async fn physical_night_recipe_does_not_publish_a_pending_main_light_target() {
    let f = Fixture::new([0, 1, 0, 0, 0, 0]).await;
    let nodes = vec![homesense_core::rule::Node {
        source: "physical-night-column".into(),
        action: homesense_core::rule::Action::Command {
            device: id("primary"),
            command: Command::SetLevel {
                level: p(10),
                transition: LevelTransition::Immediate,
            },
        },
    }];
    let prepared = crate::rule_runtime::PreparedRule::prepare_with_feedback(
        &f.admission,
        f.controller,
        vec![],
        &nodes,
        Default::default(),
        &f.feedback,
        5000,
    )
    .await
    .unwrap();
    prepared.commit(&f.admission).await.unwrap();
    let targets = f
        .admission
        .projection_levels(1, Uuid::parse_str(f.admission.boot_id()).unwrap().as_u128())
        .await
        .unwrap();
    assert!(
        !targets.contains_key(&id("primary")),
        "a raw bedside night column is not a logical main-light target"
    );
    assert_eq!(
        f.drain().await,
        vec![(
            "primary".into(),
            Command::SetLevel {
                level: p(10),
                transition: LevelTransition::Immediate
            }
        )]
    );
    f.close().await;
}

#[tokio::test]
async fn a_later_raw_recipe_retires_old_logical_target_without_erasing_restore_preference() {
    let f = Fixture::new([1, 0, 0, 35, 45, 0]).await;
    let prepared = f
        .prepare("primary", DeviceAction::Brightness { level: 80 })
        .await
        .unwrap();
    f.admission
        .submit_shared_light(f.generation.clone(), prepared, 1000)
        .await
        .unwrap();
    f.drain().await;
    let boot = Uuid::parse_str(f.admission.boot_id()).unwrap().as_u128();
    assert_eq!(
        f.admission.projection_levels(1, boot).await.unwrap()[&id("primary")],
        p(80)
    );
    let preference_before = f
        .admission
        .recorded_dimmer_levels(f.controller, f.generation.clone())
        .await
        .unwrap()[&id("primary")];
    assert_eq!(
        preference_before,
        p(80),
        "the existing explicit manual target preference is retained"
    );
    let nodes = vec![homesense_core::rule::Node {
        source: "later-physical-night".into(),
        action: homesense_core::rule::Action::Command {
            device: id("primary"),
            command: Command::SetLevel {
                level: p(10),
                transition: LevelTransition::Immediate,
            },
        },
    }];
    crate::rule_runtime::PreparedRule::prepare_with_feedback(
        &f.admission,
        f.controller,
        vec![],
        &nodes,
        Default::default(),
        &f.feedback,
        5000,
    )
    .await
    .unwrap()
    .commit(&f.admission)
    .await
    .unwrap();
    assert!(
        !f.admission
            .projection_levels(1, boot)
            .await
            .unwrap()
            .contains_key(&id("primary"))
    );
    assert_eq!(
        f.admission
            .recorded_dimmer_levels(f.controller, f.generation.clone())
            .await
            .unwrap()[&id("primary")],
        preference_before
    );
    f.drain().await;
    f.close().await;
}

#[tokio::test]
async fn existing_unsettled_reading_supply_write_prevents_shared_plan_acceptance() {
    let f = Fixture::new([0, 1, 0, 0, 0, 70]).await;
    let pending = f
        .admission
        .submit_for_revision(
            1,
            Batch {
                controller: f.controller,
                subscriptions: vec![],
                intents: vec![Intent {
                    device: id("left"),
                    command: Command::Switch(false),
                }],
            },
        )
        .await
        .unwrap();
    let prepared = f
        .prepare("secondary", DeviceAction::Power { on: true })
        .await
        .unwrap();
    assert_eq!(
        f.admission
            .submit_shared_light(f.generation.clone(), prepared, 1000)
            .await
            .unwrap_err(),
        hap::server::WriteStatus::ResourceBusy
    );
    let writes = f.drain().await;
    assert_eq!(pending.len(), 1);
    assert_eq!(writes, vec![("left".into(), Command::Switch(false))]);
    f.close().await;
}

#[tokio::test]
async fn failed_prefix_never_dispatches_the_shared_supply_activation() {
    let f = Fixture::new([0, 1, 0, 10, 0, 0]).await;
    let prepared = f
        .prepare("secondary", DeviceAction::Power { on: true })
        .await
        .unwrap();
    f.admission
        .submit_shared_light(f.generation.clone(), prepared, 1000)
        .await
        .unwrap();
    let ticket = f.admission.dispatch().await.unwrap().unwrap();
    assert_eq!(ticket.command.device_id, id("left"));
    f.admission
        .finish_write(ticket.command.id, false)
        .await
        .unwrap();
    assert!(matches!(f.admission.dispatch().await, Err(_) | Ok(None)));
    f.close().await;
}

#[test]
fn publication_preview_marks_every_shared_dependency_and_no_unrelated_binding() {
    let bytes = document();
    let mut plain: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    plain.as_object_mut().unwrap().remove("shared_lights");
    let plain = Configuration::parse(
        &serde_json::to_vec(&plain).unwrap(),
        ExecutionContext::Simulation,
    )
    .unwrap();
    let shared = Configuration::parse(&bytes, ExecutionContext::Simulation).unwrap();
    let changes = crate::configuration_preview::device_changes(&plain, &shared);
    assert_eq!(changes.len(), 6);
    assert!(
        changes
            .iter()
            .all(|change| change.changes == ["shared_light"])
    );
    assert!(!crate::configuration_preview::connection_rebuild_required(
        &plain, &shared
    ));
}

#[tokio::test]
async fn stale_input_rejects_prepare_and_raw_snapshot_is_never_rewritten_for_scenes() {
    let f = Fixture::new([0, 1, 0, 10, 100, 70]).await;
    let raw = f.feedback.snapshot();
    let mut projected = raw.clone();
    project(&f.configuration, &raw, &mut projected).unwrap();
    assert_eq!(
        projected.devices[&id("primary")].observed,
        Some(Observation::Level(p(0)))
    );
    assert_eq!(
        projected.devices[&id("secondary")].observed,
        Some(Observation::Level(p(0)))
    );
    assert_eq!(
        raw.devices[&id("primary")].observed,
        Some(Observation::Level(p(10)))
    );
    assert_eq!(
        raw.devices[&id("reading")].observed,
        Some(Observation::Level(p(70)))
    );
    f.time.store(5000, Ordering::Release);
    assert!(
        f.prepare("secondary", DeviceAction::Power { on: true })
            .await
            .is_err()
    );
    f.close().await;
}

#[tokio::test]
async fn projection_requires_both_feedback_components_and_uses_older_time() {
    let f = Fixture::new([0, 1, 0, 10, 100, 70]).await;
    let mut raw = f.feedback.snapshot();
    raw.devices.get_mut(&id("primary")).unwrap().observed_at_ms = Some(10);
    let mut logical = raw.clone();
    project(&f.configuration, &raw, &mut logical).unwrap();
    assert_eq!(logical.devices[&id("primary")].observed_at_ms, Some(0));
    raw.devices.get_mut(&id("common")).unwrap().quality = Quality::Stale;
    project(&f.configuration, &raw, &mut logical).unwrap();
    assert_eq!(logical.devices[&id("primary")].quality, Quality::Stale);
    raw.devices.get_mut(&id("common")).unwrap().observed = None;
    project(&f.configuration, &raw, &mut logical).unwrap();
    assert_eq!(logical.devices[&id("primary")].quality, Quality::Unknown);
    assert_eq!(logical.devices[&id("primary")].observed, None);
    assert_eq!(logical.devices[&id("reading")].kind, ModuleKind::Dimmer);
    f.close().await;
}

#[tokio::test]
async fn source_scope_must_cover_all_shared_dependencies_and_revocation_blocks_delivery() {
    for full in [false, true] {
        let f = Fixture::new([0, 1, 0, 0, 0, 70]).await;
        let tokens = Arc::new(crate::web_auth::Tokens::default());
        let credential = Uuid::new_v4();
        let secret = "synthetic-shared-light-test-token-only";
        tokens
            .insert(
                credential,
                secret,
                crate::web_auth::Grant {
                    controller: f.controller,
                    role: crate::web_auth::Role::Member,
                    scenes: Default::default(),
                    devices: if full {
                        IDS.into_iter().map(str::to_owned).collect()
                    } else {
                        ["secondary".into()].into()
                    },
                },
            )
            .unwrap();
        let header = format!("Bearer {secret}");
        let lease = tokens.module_off_lease(Some(&header), None, None).unwrap();
        let action = DeviceAction::Power { on: true };
        let source = crate::source_auth::Source::capture(
            lease,
            &crate::automation_control::Action::DeviceCommand {
                controller: f.controller,
                revision: 1,
                device: id("secondary"),
                action: action.clone(),
            },
            None,
        )
        .unwrap();
        let prepared = f.prepare("secondary", action).await.unwrap();
        let scoped = f.admission.with_source(Some(source));
        let result = scoped
            .submit_shared_light(f.generation.clone(), prepared, 1000)
            .await;
        if full {
            assert!(result.is_ok());
            tokens.revoke(credential).unwrap();
        } else {
            assert_eq!(
                result.unwrap_err(),
                hap::server::WriteStatus::InsufficientPrivileges
            );
        }
        assert!(f.admission.dispatch().await.unwrap().is_none());
        f.close().await;
    }
}
