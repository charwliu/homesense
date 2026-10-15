use homesense_config::Configuration;
use homesense_core::{DeviceId, ExecutionContext};
use homesensed::module_all_off::{self, Error, LightingReview};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    io::{self, Read, Write},
    sync::{Arc, Mutex},
};
fn fixture() -> Value {
    let mut modules = vec![];
    let mut devices = vec![];
    for (id, address, profile) in [
        ("relay", "27", "r8i8_din_pro_v2_7_7"),
        ("dimmer", "38", "ad8_din_rs485_v2_2_1"),
    ] {
        modules.push(json!({"id":id,"address":address,"profile":profile,"verification":{"status":"simulated"}}));
        for channel in 1..=8 {
            devices.push(json!({"id":format!("{id}:{channel}"),"name":"Synthetic light","module_id":id,"channel":channel,"enabled":true}));
        }
    }
    json!({"schema_version":2,"revision":1,"gateways":[{"id":"bus","endpoint":"127.0.0.1:1","modules":modules}],"devices":devices})
}
fn parse(doc: &Value) -> Configuration {
    Configuration::parse(
        &serde_json::to_vec(doc).unwrap(),
        ExecutionContext::Simulation,
    )
    .unwrap()
}
fn policy() -> (Vec<LightingReview>, BTreeSet<DeviceId>) {
    let mut all = BTreeSet::new();
    let mut reviews = vec![];
    for module in ["relay", "dimmer"] {
        let ids: Vec<_> = (1..=8)
            .map(|n| DeviceId::new(format!("{module}:{n}")).unwrap())
            .collect();
        all.extend(ids.iter().cloned());
        reviews.push(
            LightingReview::new(
                module.into(),
                ids,
                "Synthetic all eight ports explicitly classified as lighting".into(),
            )
            .unwrap(),
        );
    }
    (reviews, all)
}
#[derive(Clone, Default)]
struct Memory(Arc<Mutex<Vec<u8>>>);
impl Write for Memory {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
impl Read for Memory {
    fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
        Err(io::ErrorKind::WouldBlock.into())
    }
}
#[test]
fn sixteen_reviewed_outputs_make_two_module_frames_without_per_channel_writes() {
    let config = parse(&fixture());
    let (reviews, authorized) = policy();
    let plan =
        module_all_off::plan(&config, &reviews, &authorized, ExecutionContext::Simulation).unwrap();
    assert_eq!(plan.revision(), 1);
    assert_eq!(plan.modules().len(), 2);
    let memory = Memory::default();
    let mut connection = hb_transport::Connection::for_simulation(memory.clone(), 9);
    for module in plan.modules() {
        assert_eq!(module.gateway_id(), "bus");
        assert_eq!(module.devices().len(), 8);
        let frame = hb_transport::PreparedFrame::prepare_module_all_off(
            module.module(),
            ExecutionContext::Simulation,
            9,
        )
        .unwrap();
        connection.send(&frame, 100, || 1).unwrap();
    }
    assert_eq!(
        *memory.0.lock().unwrap(),
        vec![0xAE, 0x38, 0, 0, 0xEE, 0xAE, 0x27, 0, 0xF2, 0xEE]
    );
}
#[test]
fn one_missing_permission_rejects_the_entire_plan() {
    let config = parse(&fixture());
    let (reviews, mut authorized) = policy();
    authorized.remove(&DeviceId::new("relay:8").unwrap());
    assert_eq!(
        module_all_off::plan(&config, &reviews, &authorized, ExecutionContext::Simulation)
            .unwrap_err(),
        Error::Unauthorized
    );
}
#[test]
fn non_lighting_unreviewed_or_unbound_port_cannot_be_hidden() {
    let mut doc = fixture();
    doc["devices"].as_array_mut().unwrap().remove(7);
    let config = parse(&doc);
    let (reviews, authorized) = policy();
    assert_eq!(
        module_all_off::plan(&config, &reviews, &authorized, ExecutionContext::Simulation)
            .unwrap_err(),
        Error::InvalidLightingBinding
    );
    let incomplete = LightingReview::new(
        "relay".into(),
        (1..=7)
            .map(|n| DeviceId::new(format!("relay:{n}")).unwrap())
            .collect(),
        "Only seven lighting ports; eighth is non-lighting".into(),
    )
    .unwrap();
    assert_eq!(
        module_all_off::plan(
            &parse(&fixture()),
            &[incomplete],
            &authorized,
            ExecutionContext::Simulation
        )
        .unwrap_err(),
        Error::IncompleteCoverage
    );
}
#[test]
fn disabled_binding_cannot_be_silently_controlled_by_module_scope() {
    let mut doc = fixture();
    doc["devices"][7]["enabled"] = false.into();
    let (reviews, authorized) = policy();
    assert_eq!(
        module_all_off::plan(
            &parse(&doc),
            &reviews,
            &authorized,
            ExecutionContext::Simulation
        )
        .unwrap_err(),
        Error::InvalidLightingBinding
    );
}
#[test]
fn module_stable_id_and_profile_are_revalidated_without_address_fallback() {
    let (reviews, authorized) = policy();
    let config = parse(&fixture());
    let plan =
        module_all_off::plan(&config, &reviews, &authorized, ExecutionContext::Simulation).unwrap();
    let mut moved = fixture();
    moved["gateways"][0]["modules"][0]["address"] = "42".into();
    assert_eq!(
        plan.revalidate(&parse(&moved), &authorized, ExecutionContext::Simulation),
        Err(Error::ConfigurationChanged)
    );
    let mut revised = fixture();
    revised["revision"] = 2.into();
    assert_eq!(
        plan.revalidate(&parse(&revised), &authorized, ExecutionContext::Simulation),
        Err(Error::ConfigurationChanged)
    );
    let mut renamed = fixture();
    renamed["gateways"][0]["modules"][0]["id"] = "renamed".into();
    for d in renamed["devices"].as_array_mut().unwrap() {
        if d["module_id"] == "relay" {
            d["module_id"] = "renamed".into();
        }
    }
    assert_eq!(
        module_all_off::plan(
            &parse(&renamed),
            &reviews,
            &authorized,
            ExecutionContext::Simulation
        )
        .unwrap_err(),
        Error::UnknownModule
    );
    let mut unsupported = fixture();
    unsupported["gateways"][0]["modules"][0]["profile"] = "gt_c8_rl_v2_8_2_2".into();
    assert_eq!(
        module_all_off::plan(
            &parse(&unsupported),
            &reviews,
            &authorized,
            ExecutionContext::Simulation
        )
        .unwrap_err(),
        Error::UnsupportedProfile
    );
}
#[test]
fn duplicate_or_empty_review_and_live_simulated_module_are_refused() {
    let config = parse(&fixture());
    let (reviews, authorized) = policy();
    assert_eq!(
        module_all_off::plan(&config, &[], &authorized, ExecutionContext::Simulation).unwrap_err(),
        Error::EmptyRequest
    );
    assert_eq!(
        module_all_off::plan(
            &config,
            &[reviews[0].clone(), reviews[0].clone()],
            &authorized,
            ExecutionContext::Simulation
        )
        .unwrap_err(),
        Error::DuplicateModule
    );
    assert_eq!(
        module_all_off::plan(&config, &reviews, &authorized, ExecutionContext::Live).unwrap_err(),
        Error::UnverifiedModule
    );
    assert!(
        LightingReview::new(
            "relay".into(),
            vec![DeviceId::new("relay:1").unwrap()],
            "".into()
        )
        .is_err()
    );
    assert!(
        LightingReview::new(
            "relay".into(),
            vec![DeviceId::new("relay:1").unwrap(); 8],
            "review".into()
        )
        .is_err()
    );
}
