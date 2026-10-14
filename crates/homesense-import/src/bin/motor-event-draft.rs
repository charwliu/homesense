//! Opt-in, exact-source, disabled motor event export. No transport dependency.
use homesense_core::{DeviceId, ExecutionContext, ModuleKind};
use std::{collections::BTreeMap, io::Read};
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Binding {
    device: String,
    kind: String,
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Review {
    source_key: String,
    source_xml_sha256: String,
    source_target: String,
    device: String,
    mapping_review: String,
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Reviews {
    configuration_revision: u64,
    reviews: Vec<Review>,
}
fn read(path: &str, max: u64) -> Result<String, String> {
    let mut s = String::new();
    std::fs::File::open(path)
        .map_err(|e| e.to_string())?
        .take(max + 1)
        .read_to_string(&mut s)
        .map_err(|e| e.to_string())?;
    if s.len() as u64 > max {
        return Err("input exceeds bound".into());
    }
    Ok(s)
}
fn run() -> Result<(), String> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if !(args.len() == 5 || args.len() == 6 && args[5] == "--simulation") {
        return Err("usage: motor-event-draft PROJECT.xml SOURCE_KEY CONFIG.json BINDINGS.json REVIEWS.json [--simulation]".into());
    }
    let project = read(&args[0], 32 * 1024 * 1024)?;
    let doc = roxmltree::Document::parse(&project).map_err(|e| e.to_string())?;
    let matches = doc
        .root_element()
        .children()
        .find(|n| n.has_tag_name("event_mgr"))
        .into_iter()
        .flat_map(|m| m.children().filter(|n| n.has_tag_name("event")))
        .filter(|n| {
            format!(
                "{}:{}",
                n.children()
                    .find(|n| n.has_tag_name("deviceid"))
                    .and_then(|n| n.text())
                    .unwrap_or(""),
                n.children()
                    .find(|n| n.has_tag_name("eventid"))
                    .and_then(|n| n.text())
                    .unwrap_or("")
            ) == args[1]
        })
        .collect::<Vec<_>>();
    if matches.len() != 1 {
        return Err("exact source event must be unique".into());
    }
    let config = homesense_config::Configuration::parse(
        read(&args[2], 4 * 1024 * 1024)?.as_bytes(),
        if args.len() == 6 {
            ExecutionContext::Simulation
        } else {
            ExecutionContext::Live
        },
    )
    .map_err(|e| e.to_string())?;
    let inputs: BTreeMap<String, Binding> =
        serde_json::from_str(&read(&args[3], 1024 * 1024)?).map_err(|e| e.to_string())?;
    let bindings = inputs
        .into_iter()
        .map(|(key, b)| {
            let kind = match b.kind.as_str() {
                "switch" => ModuleKind::Switch,
                "dimmer" => ModuleKind::Dimmer,
                "motor" => ModuleKind::Motor,
                "keypad" => ModuleKind::Keypad,
                _ => return Err("unknown binding kind".into()),
            };
            Ok((
                key,
                homesense_import::semantic::Binding {
                    device: DeviceId::new(b.device).map_err(|e| e.to_string())?,
                    kind,
                },
            ))
        })
        .collect::<Result<BTreeMap<_, _>, String>>()?;
    let reviews: Reviews =
        serde_json::from_str(&read(&args[4], 1024 * 1024)?).map_err(|e| e.to_string())?;
    if reviews.configuration_revision != config.revision() {
        return Err("motor mapping review configuration revision mismatch".into());
    }
    let reviews = reviews
        .reviews
        .into_iter()
        .map(|r| {
            Ok(homesense_import::semantic::MotorReview {
                source_key: r.source_key,
                source_xml_sha256: r.source_xml_sha256,
                source_target: r.source_target,
                device: DeviceId::new(r.device).map_err(|e| e.to_string())?,
                mapping_review: r.mapping_review,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    let options =
        homesense_import::semantic::MotorOptions::reviewed_project(&config, &project, &reviews)?;
    let event = homesense_import::semantic::compile_event_with_motors(
        &project[matches[0].range()],
        &bindings,
        options,
    )?;
    println!(
        "{}",
        serde_json::to_string_pretty(&homesense_import::event_export::draft(&event)?)
            .map_err(|e| e.to_string())?
    );
    Ok(())
}
fn main() {
    if let Err(e) = run() {
        eprintln!("{e}");
        std::process::exit(1);
    }
}
