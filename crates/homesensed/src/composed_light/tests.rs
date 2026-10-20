use super::*;
use serde_json::json;

fn id(s: &str) -> DeviceId {
    DeviceId::new(s).unwrap()
}
fn p(n: u8) -> Percent {
    Percent::new(n).unwrap()
}
fn south() -> Group {
    serde_json::from_value(json!({"id":"south","review_evidence":"reviewed synthetic two-bank matrix",
        "lights":[
            {"id":"left","name":"Left","members":[{"supply":"bed","dimmer":"left"}],"default_on_level":100,"excludes_on":["ceiling"]},
            {"id":"right","name":"Right","members":[{"supply":"bed","dimmer":"right"}],"default_on_level":100,"excludes_on":["ceiling"]},
            {"id":"night","name":"Night","members":[{"supply":"bed","dimmer":"night"}],"default_on_level":100,"excludes_on":["ceiling"]},
            {"id":"ceiling","name":"Downlight","members":[{"supply":"ceiling","dimmer":"night"}],"default_on_level":100,"excludes_on":["left","right","night"]}
        ]})).unwrap()
}
fn state(bed: bool, ceiling: bool, l: u8, r: u8, n: u8) -> Matrix {
    Matrix {
        powers: BTreeMap::from([(id("bed"), bed), (id("ceiling"), ceiling)]),
        levels: BTreeMap::from([(id("left"), p(l)), (id("right"), p(r)), (id("night"), p(n))]),
    }
}
fn apply(mut state: Matrix, writes: Vec<Intent>) -> Matrix {
    for intent in writes {
        match intent.command {
            Command::Switch(on) => {
                state.powers.insert(intent.device, on);
            }
            Command::SetLevel {
                level,
                transition: LevelTransition::Immediate,
            } => {
                state.levels.insert(intent.device, level);
            }
            _ => panic!("unrequested physical command"),
        }
    }
    state
}
#[test]
fn composed_independent_reading_off_preserves_peer_and_night() {
    let g = south();
    let b = state(true, false, 100, 65, 10);
    let a = apply(
        b.clone(),
        plan(&g, g.light(&id("left")).unwrap(), &b, p(0)).unwrap(),
    );
    assert!(a.powers[&id("bed")]);
    assert_eq!(a.levels[&id("right")], p(65));
    assert_eq!(a.levels[&id("night")], p(10));
    assert_eq!(a.levels[&id("left")], p(0));
}
#[test]
fn composed_last_light_off_releases_shared_supply() {
    let g = south();
    let b = state(true, false, 100, 0, 0);
    let a = apply(
        b.clone(),
        plan(&g, g.light(&id("left")).unwrap(), &b, p(0)).unwrap(),
    );
    assert!(!a.powers[&id("bed")]);
}
#[test]
fn composed_reviewed_exclusion_drops_supply_before_shared_column_changes() {
    let g = south();
    let b = state(true, false, 100, 65, 10);
    let writes = plan(&g, g.light(&id("ceiling")).unwrap(), &b, p(100)).unwrap();
    assert_eq!(writes[0].device, id("bed"));
    assert_eq!(writes[0].command, Command::Switch(false));
    let a = apply(b, writes);
    assert!(a.powers[&id("ceiling")]);
    assert!(!a.powers[&id("bed")]);
    assert_eq!(a.levels[&id("night")], p(100));
    assert_eq!(a.levels[&id("left")], p(0));
    assert_eq!(a.levels[&id("right")], p(0));
}
#[test]
fn composed_unreviewed_interference_is_rejected_instead_of_partial_control() {
    let mut g = south();
    g.lights.iter_mut().for_each(|l| l.excludes_on.clear());
    let b = state(true, false, 100, 0, 0);
    assert!(plan(&g, g.light(&id("ceiling")).unwrap(), &b, p(100)).is_err());
}
#[test]
fn composed_reading_exits_ceiling_without_lighting_night() {
    let g = south();
    let b = state(false, true, 0, 0, 100);
    let a = apply(
        b.clone(),
        plan(&g, g.light(&id("left")).unwrap(), &b, p(45)).unwrap(),
    );
    assert!(!a.powers[&id("ceiling")]);
    assert!(a.powers[&id("bed")]);
    assert_eq!(a.levels[&id("left")], p(45));
    assert_eq!(a.levels[&id("night")], p(0));
}
#[test]
fn composed_all_binary_states_admit_only_exact_preservation_or_reviewed_exclusion() {
    let g = south();
    for bits in 0..32 {
        let b = state(
            bits & 1 != 0,
            bits & 2 != 0,
            if bits & 4 != 0 { 35 } else { 0 },
            if bits & 8 != 0 { 65 } else { 0 },
            if bits & 16 != 0 { 10 } else { 0 },
        );
        for target in &g.lights {
            for level in [0, 10, 100] {
                let Ok(writes) = plan(&g, target, &b, p(level)) else {
                    continue;
                };
                let a = apply(b.clone(), writes);
                for light in &g.lights {
                    for member in &light.members {
                        let expected = if light.id == target.id {
                            level > 0
                        } else if level > 0 && target.excludes_on.contains(&light.id) {
                            false
                        } else {
                            b.lit(member)
                        };
                        assert_eq!(
                            a.lit(member),
                            expected,
                            "bits={bits} target={:?} level={level}",
                            target.id
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn composed_card_activation_accepts_shared_lighting_without_executing_scene_exclusions() {
    let mut g = south();
    g.allow_shared_side_effects = true;
    g.lights.iter_mut().for_each(|l| l.excludes_on.clear());
    let before = state(true, false, 65, 0, 0);
    let after = apply(
        before.clone(),
        plan(&g, g.light(&id("ceiling")).unwrap(), &before, p(100)).unwrap(),
    );
    assert_eq!(after.levels[&id("left")], p(65));
    assert!(after.powers[&id("bed")]);
    assert!(after.powers[&id("ceiling")]);
    assert_eq!(after.levels[&id("night")], p(100));
    // Closing the separate supply must retain the shared column and bedside lights.
    let closed = apply(
        after.clone(),
        plan(&g, g.light(&id("ceiling")).unwrap(), &after, p(0)).unwrap(),
    );
    assert!(!closed.powers[&id("ceiling")]);
    assert!(closed.powers[&id("bed")]);
    assert_eq!(closed.levels[&id("night")], p(100));
    assert_eq!(closed.levels[&id("left")], p(65));
}

#[test]
fn composed_both_shared_resources_in_use_cannot_close_another_active_lamp() {
    let mut g = south();
    g.allow_shared_side_effects = true;
    g.lights.iter_mut().for_each(|l| l.excludes_on.clear());
    let before = state(true, true, 65, 0, 100);
    assert!(plan(&g, g.light(&id("night")).unwrap(), &before, p(0)).is_err());
}

fn config_value() -> serde_json::Value {
    let group = south();
    let lights=group.lights.iter().map(|l|json!({"id":l.id.as_str(),"name":l.name,
        "members":l.members.iter().map(|m|json!({"supply":m.supply.as_str(),"dimmer":m.dimmer.as_str()})).collect::<Vec<_>>(),
        "default_on_level":100,"excludes_on":l.excludes_on.iter().map(|id|id.as_str()).collect::<Vec<_>>()})).collect::<Vec<_>>();
    json!({"schema_version":2,"revision":1,"rooms":[{"id":"room","name":"room"}],
        "gateways":[{"id":"bus","endpoint":"127.0.0.1:1","modules":[
            {"id":"relay","address":"31","profile":"r8i8_din_pro_v2_7_7","verification":{"status":"simulated"}},
            {"id":"dimmer","address":"32","profile":"ad8_din_rs485_v2_2_1","verification":{"status":"simulated"}}]}],
        "devices":[
            {"id":"bed","name":"physical bed supply","room":"room","module_id":"relay","channel":1,"enabled":true},
            {"id":"ceiling","name":"physical ceiling supply","room":"room","module_id":"relay","channel":2,"enabled":true},
            {"id":"left","name":"physical left dimmer","room":"room","module_id":"dimmer","channel":1,"enabled":true,"default_on_level":100},
            {"id":"right","name":"physical right dimmer","room":"room","module_id":"dimmer","channel":2,"enabled":true,"default_on_level":100},
            {"id":"night","name":"physical shared dimmer","room":"room","module_id":"dimmer","channel":3,"enabled":true,"default_on_level":100}],
        "composed_lights":[{"id":"south","review_evidence":"synthetic reviewed matrix","lights":lights}]})
}
fn configuration() -> Configuration {
    Configuration::parse(
        &serde_json::to_vec(&config_value()).unwrap(),
        homesense_core::ExecutionContext::Simulation,
    )
    .unwrap()
}

#[test]
fn composed_scene_details_expand_affected_logical_lamps_without_raw_supply_duplicates() {
    let config = configuration();
    let document=homesense_config::rule_document::Document::parse(&serde_json::to_vec(&json!({
        "schema_version":1,"configuration_revision":1,"variables":[],"rules":[],"immediate_scenes":[{
            "id":"physical-scene","enabled":true,"review_evidence":"synthetic physical scene",
            "activate":[
                {"source":"physical power","action":{"kind":"command","device":"bed","command":{"kind":"switch","on":true}}},
                {"source":"physical column","action":{"kind":"command","device":"night","command":{"kind":"level","value":10,"ramp":false}}}
            ]
        }]
    })).unwrap(),&config).unwrap();
    let members = crate::scene_lights::members(&config, &document, "physical-scene").unwrap();
    assert_eq!(
        members.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
        ["ceiling", "left", "night", "right"]
    );
    assert_eq!(
        members.iter().map(|m| m.name.as_str()).collect::<Vec<_>>(),
        ["Downlight", "Left", "Night", "Right"]
    );
}

#[test]
fn composed_configuration_rejects_wrong_kind_duplicate_edges_and_cross_room_wiring() {
    for mutate in 0..5 {
        let mut v = config_value();
        match mutate {
            0 => v["composed_lights"][0]["lights"][0]["members"][0]["supply"] = json!("left"),
            1 => {
                v["composed_lights"][0]["lights"][1]["members"] =
                    v["composed_lights"][0]["lights"][0]["members"].clone()
            }
            2 => v["devices"][0]["enabled"] = json!(false),
            3 => v["composed_lights"][0]["lights"][0]["excludes_on"] = json!(["not-a-light"]),
            _ => v["devices"][0]["room"] = serde_json::Value::Null,
        }
        assert!(
            Configuration::parse(
                &serde_json::to_vec(&v).unwrap(),
                homesense_core::ExecutionContext::Simulation
            )
            .is_err(),
            "mutation{mutate}"
        );
    }
}

struct Fixture {
    root: std::path::PathBuf,
    admission: Admission,
    config: Configuration,
    feedback: Feedback,
    controller: Uuid,
    generation: TaskGeneration,
    time: &'static std::sync::atomic::AtomicU64,
}
impl Fixture {
    async fn new(values: [u8; 5]) -> Self {
        use homesense_core::ExecutionContext;
        let root = std::env::temp_dir().join(format!("composed-light-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let bytes = serde_json::to_vec(&config_value()).unwrap();
        let path = root.join("state.sqlite");
        let mut store = homesense_storage::Store::open(&path).unwrap();
        store
            .publish_configuration(None, &bytes, ExecutionContext::Simulation, &[])
            .unwrap();
        drop(store);
        let controller = Uuid::new_v4();
        let time = Box::leak(Box::new(std::sync::atomic::AtomicU64::new(0)));
        let admission = Admission::open_with_test_time(
            path,
            crate::admission::Options {
                context: ExecutionContext::Simulation,
                ..Default::default()
            },
            move |id| id == controller,
            time,
        )
        .await
        .unwrap();
        admission.synchronized(1, 1).await.unwrap();
        let generation = admission.task_generation().await.unwrap();
        let config = configuration();
        let feedback = Feedback::new(
            &config,
            ExecutionContext::Simulation,
            Uuid::parse_str(admission.boot_id()).unwrap().as_u128(),
            1,
        )
        .unwrap();
        let mut f = Self {
            root,
            admission,
            config,
            feedback,
            controller,
            generation,
            time,
        };
        f.sample(values, 0);
        f
    }
    fn sample(&mut self, values: [u8; 5], at: u64) {
        let frames = values
            .into_iter()
            .enumerate()
            .map(|(i, n)| hb_protocol::RawFrame {
                address: if i < 2 { 0x31 } else { 0x32 },
                command: 0xd1 + if i < 2 { i as u8 } else { (i - 2) as u8 },
                value: if i < 2 {
                    if n > 0 { 0xf1 } else { 0xf2 }
                } else {
                    n
                },
            })
            .collect();
        self.feedback
            .apply(&crate::transport_owner::GatewayRead {
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
    async fn prepared(
        &self,
        target: &str,
        action: DeviceAction,
    ) -> Result<crate::shared_light::Prepared, String> {
        crate::shared_light::prepare(
            &self.admission,
            self.controller,
            self.generation.clone(),
            &id(target),
            &action,
            &self.feedback,
            5000,
        )
        .await?
        .ok_or("missing".into())
    }
    async fn drain(&self) -> Vec<(DeviceId, Command)> {
        let mut out = vec![];
        while let Some(t) = self.admission.dispatch().await.unwrap() {
            out.push((t.command.device_id.clone(), t.command.command));
            self.admission
                .finish_write(t.command.id, true)
                .await
                .unwrap();
            self.time
                .fetch_add(20, std::sync::atomic::Ordering::Release);
        }
        out
    }
    async fn close(self) {
        self.admission.shutdown().await.unwrap();
        std::fs::remove_dir_all(self.root).unwrap();
    }
}

#[tokio::test]
async fn composed_actual_admission_and_catalog_project_four_dimmable_cards_without_fake_bindings() {
    let f = Fixture::new([1, 0, 100, 65, 10]).await;
    let prepared = f
        .prepared("left", DeviceAction::Power { on: false })
        .await
        .unwrap();
    f.admission
        .submit_shared_light(f.generation.clone(), prepared, 1000)
        .await
        .unwrap();
    assert_eq!(
        f.drain().await,
        vec![(
            id("left"),
            Command::SetLevel {
                level: p(0),
                transition: LevelTransition::Immediate
            }
        )]
    );
    let view = crate::management_devices::snapshot(
        &f.admission,
        f.controller,
        f.generation.clone(),
        ["bed", "ceiling", "left", "right", "night"]
            .map(id)
            .to_vec(),
        &f.feedback,
    )
    .await
    .unwrap();
    assert_eq!(view.devices.iter().filter(|d| d.catalog_visible).count(), 4);
    let ceiling = view.devices.iter().find(|d| d.id == id("ceiling")).unwrap();
    assert_eq!(ceiling.state.kind, ModuleKind::Dimmer);
    assert_eq!(ceiling.reported, Some(Observation::Level(p(0))));
    assert_eq!(ceiling.name, "Downlight");
    assert_eq!(
        f.config.binding(&id("ceiling")).unwrap().1.kind(),
        ModuleKind::Switch
    );
    f.close().await;
}

#[tokio::test]
async fn composed_restore_distinguishes_night_from_other_bank_on_the_same_dimmer() {
    let mut f = Fixture::new([0, 1, 0, 0, 35]).await;
    let levels = f
        .admission
        .remember_levels(1, f.feedback.snapshot(), 5000)
        .await
        .unwrap();
    assert_eq!(levels.get(&id("ceiling")), Some(&p(35)));
    assert!(!levels.contains_key(&id("night")));
    f.sample([1, 0, 0, 0, 10], 100);
    f.time.store(100, std::sync::atomic::Ordering::Release);
    let prepared = f
        .prepared("ceiling", DeviceAction::Power { on: true })
        .await
        .unwrap();
    f.admission
        .submit_shared_light(f.generation.clone(), prepared, 1000)
        .await
        .unwrap();
    let writes = f.drain().await;
    assert!(writes.contains(&(
        id("night"),
        Command::SetLevel {
            level: p(35),
            transition: LevelTransition::Immediate
        }
    )));
    assert!(!writes.contains(&(
        id("night"),
        Command::SetLevel {
            level: p(10),
            transition: LevelTransition::Immediate
        }
    )));
    f.close().await;
}

#[tokio::test]
async fn composed_capture_expiry_and_physical_change_fence_the_whole_plan() {
    let mut f = Fixture::new([1, 0, 100, 65, 10]).await;
    let prepared = f
        .prepared("left", DeviceAction::Power { on: false })
        .await
        .unwrap();
    f.sample([1, 0, 100, 20, 10], 10);
    f.time.store(10, std::sync::atomic::Ordering::Release);
    // The prepared lease still references the old complete matrix version.
    assert!(
        f.admission
            .submit_shared_light(f.generation.clone(), prepared, 1000)
            .await
            .is_err()
    );
    f.time.store(6000, std::sync::atomic::Ordering::Release);
    assert!(
        f.prepared("left", DeviceAction::Power { on: false })
            .await
            .is_err()
    );
    assert!(f.drain().await.is_empty());
    f.close().await;
}
