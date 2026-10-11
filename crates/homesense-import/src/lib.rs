//! Read-only Control4 project XML inventory. No imported object is enabled;
//! driver category is not evidence of a verified physical protocol profile.
pub mod configuration;
pub mod program;
pub mod scene;
use roxmltree::{Document, Node};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
#[derive(Debug, Serialize)]
pub struct Device {
    pub control4_id: String,
    pub stable_id: String,
    pub name: String,
    pub room: Option<String>,
    pub kind: String,
    pub address: u8,
    pub channel: Option<u8>,
    pub enabled: bool,
    pub verification: &'static str,
}
#[derive(Debug, Serialize)]
pub struct Reference {
    pub owner: String,
    pub target: String,
    pub resolved: bool,
    pub disposition: &'static str,
}
#[derive(Debug, Serialize)]
pub struct Source {
    pub format: &'static str,
    pub sha256: String,
    pub project_xml_sha256: String,
}
#[derive(Debug, Serialize)]
pub struct Report {
    pub source: Option<Source>,
    pub schema_version: u32,
    pub item_count: usize,
    pub devices: Vec<Device>,
    pub scene_count: usize,
    pub scenes: Vec<scene::Scene>,
    pub event_count: usize,
    pub unsupported_code_nodes: usize,
    pub structural_code_nodes: usize,
    pub programs: Vec<program::Program>,
    pub references: Vec<Reference>,
    pub diagnostics: Vec<String>,
}
fn text<'a>(node: Node<'a, '_>, name: &str) -> Option<&'a str> {
    node.children()
        .find(|n| n.has_tag_name(name))
        .and_then(|n| n.text())
        .map(str::trim)
}
fn required<'a>(node: Node<'a, '_>, name: &str) -> Result<&'a str, String> {
    text(node, name)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("missing {name}"))
}
fn parse_xml(xml: &str) -> Result<Document<'_>, String> {
    if xml.len() > 32 * 1024 * 1024 {
        return Err("XML exceeds 32 MiB".into());
    }
    let doc = Document::parse(xml).map_err(|e| e.to_string())?;
    if doc.descendants().count() > 500_000 {
        return Err("XML node budget exceeded".into());
    }
    Ok(doc)
}
pub fn project_xml(xml: &str) -> Result<Report, String> {
    let document = parse_xml(xml)?;
    let system = document
        .root_element()
        .children()
        .find(|n| n.has_tag_name("systemitems"))
        .ok_or("missing systemitems")?;
    let mut items = BTreeMap::new();
    for item in system.descendants().filter(|n| n.has_tag_name("item")) {
        let id = required(item, "id")?;
        if !id.bytes().all(|b| b.is_ascii_digit()) || items.insert(id, item).is_some() {
            return Err(format!("invalid or duplicate item ID {id}"));
        }
    }
    let mut report = Report {
        source: None,
        schema_version: 1,
        item_count: items.len(),
        devices: vec![],
        scene_count: 0,
        scenes: Vec::new(),
        event_count: 0,
        unsupported_code_nodes: 0,
        structural_code_nodes: 0,
        programs: Vec::new(),
        references: vec![],
        diagnostics: vec![],
    };
    let mut endpoints = BTreeSet::new();
    for (&id, &item) in &items {
        let driver = item
            .children()
            .find(|n| n.has_tag_name("itemdata"))
            .and_then(|n| text(n, "config_data_file"))
            .unwrap_or("");
        let kind = match driver {
            "GJ-TECH.CN_Switch_V2.c4z" => Some("switch"),
            "GJ-TECH.CN_Dimmer_V2.c4z" => Some("dimmer"),
            "GJ-TECH.CN_Blinds.c4z" => Some("motor"),
            "GJ-TECH.CN_Keypad.c4z" => Some("keypad"),
            _ => None,
        };
        let state = match text(item, "state")
            .filter(|s| !s.is_empty())
            .map(parse_xml)
            .transpose()
        {
            Ok(state) => state,
            Err(_) => {
                report
                    .diagnostics
                    .push(format!("item {id}: invalid embedded state; not imported"));
                continue;
            }
        };
        if let Some(state) = &state {
            for scene in state.descendants().filter(|n| n.has_tag_name("AdvScene")) {
                let parsed = scene::parse(scene, id)?;
                if report
                    .scenes
                    .iter()
                    .any(|s| s.stable_id == parsed.stable_id)
                {
                    return Err("duplicate scene ID within owner".into());
                }
                let owner = parsed.stable_id.clone();
                report.scenes.push(parsed);
                report.scene_count += 1;
                for member in scene
                    .descendants()
                    .filter(|n| n.has_tag_name("AdvSceneMember"))
                {
                    let target = required(member, "device_id")?;
                    report.references.push(Reference {
                        owner: owner.clone(),
                        target: target.into(),
                        resolved: items.contains_key(target),
                        disposition: "disabled_pending_scene_translation",
                    });
                }
            }
        }
        let Some(kind) = kind else {
            continue;
        };
        let state = state.ok_or_else(|| format!("HopeBeauty item {id}: missing state"))?;
        // Whitelist bus mapping fields; activation keys and unrelated properties
        // are never copied to the report.
        let mut properties = BTreeMap::new();
        for property in state.descendants().filter(|n| n.has_tag_name("property")) {
            if let Some(name @ ("Device ID" | "Channel")) = text(property, "name")
                && properties
                    .insert(name, required(property, "value")?)
                    .is_some()
            {
                return Err(format!("item {id}: duplicate mapping property"));
            }
        }
        let address = properties
            .get("Device ID")
            .ok_or_else(|| format!("item {id}: missing address"))?;
        if address.len() != 2 {
            return Err(format!("item {id}: expected two hex address digits"));
        }
        let address = u8::from_str_radix(address, 16)
            .map_err(|_| format!("item {id}: invalid hex address"))?;
        if address == 0 {
            return Err(format!("item {id}: zero bus address"));
        }
        let channel = if kind == "keypad" {
            None
        } else {
            let channel: u8 = properties
                .get("Channel")
                .ok_or_else(|| format!("item {id}: missing channel"))?
                .parse()
                .map_err(|_| format!("item {id}: invalid channel"))?;
            if !(1..=16).contains(&channel) {
                return Err(format!("item {id}: invalid channel"));
            }
            Some(channel)
        };
        if !endpoints.insert((address, channel)) {
            report.diagnostics.push(format!(
                "item {id}: duplicate bus endpoint; requires mapping review"
            ));
        }
        let room = item
            .ancestors()
            .skip(1)
            .find(|n| n.has_tag_name("item") && text(*n, "type") == Some("8"))
            .and_then(|n| text(n, "name"))
            .map(str::to_owned);
        report.devices.push(Device {
            control4_id: id.into(),
            stable_id: format!("control4:{id}"),
            name: required(item, "name")?.into(),
            room,
            kind: kind.into(),
            address,
            channel,
            enabled: false,
            verification: "pending",
        });
    }
    if let Some(manager) = document
        .root_element()
        .children()
        .find(|n| n.has_tag_name("event_mgr"))
    {
        for event in manager.children().filter(|n| n.has_tag_name("event")) {
            report.event_count += 1;
            let target = required(event, "deviceid")?;
            let owner = format!("event:{target}:{}", required(event, "eventid")?);
            report.references.push(Reference {
                owner: owner.clone(),
                target: target.into(),
                resolved: items.contains_key(target),
                disposition: "disabled_pending_program_translation",
            });
            let mut seen = BTreeSet::new();
            let nodes = event
                .children()
                .filter(|n| n.has_tag_name("codeitem"))
                .map(|n| program::code(n, true, 0, &mut seen))
                .collect::<Result<Vec<_>, _>>()?;
            if seen.len()
                != event
                    .descendants()
                    .filter(|n| n.has_tag_name("codeitem"))
                    .count()
            {
                return Err(format!(
                    "{owner}: program traversal did not cover all source nodes"
                ));
            }
            let mut pending: Vec<_> = nodes.iter().collect();
            while let Some(code) = pending.pop() {
                pending.extend(code.children.iter());
                if let Some(target) = &code.target {
                    report.unsupported_code_nodes += 1;
                    report.references.push(Reference {
                        owner: format!("{owner}/code:{}", code.id),
                        target: target.clone(),
                        resolved: items.contains_key(target.as_str())
                            || matches!(target.as_str(), "100000" | "100001"),
                        disposition: "unsupported_preserve_source",
                    });
                } else {
                    report.structural_code_nodes += 1;
                }
            }
            report.programs.push(program::Program {
                owner,
                enabled: false,
                nodes,
            });
        }
    }
    Ok(report)
}

/// Import an original C4P ZIP or an extracted XML without extracting archive
/// paths to disk. Only the exact root project.xml member is read.
pub fn import_bytes(bytes: &[u8]) -> Result<Report, String> {
    use std::io::{Cursor, Read};
    if bytes.len() > 64 * 1024 * 1024 {
        return Err("input exceeds 64 MiB".into());
    }
    let (format, xml) = if bytes.starts_with(b"PK") {
        let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).map_err(|e| e.to_string())?;
        if archive.len() > 10000 {
            return Err("archive entry limit exceeded".into());
        }
        let file = archive
            .by_name("project.xml")
            .map_err(|_| "missing readable root project.xml")?;
        if !file.is_file() || file.encrypted() || file.size() > 32 * 1024 * 1024 {
            return Err("invalid or oversized project.xml".into());
        }
        if !matches!(
            file.compression(),
            zip::CompressionMethod::Stored | zip::CompressionMethod::Deflated
        ) {
            return Err("unsupported project.xml compression".into());
        }
        let mut xml = Vec::new();
        file.take(32 * 1024 * 1024 + 1)
            .read_to_end(&mut xml)
            .map_err(|e| e.to_string())?;
        if xml.len() > 32 * 1024 * 1024 {
            return Err("expanded XML exceeds limit".into());
        }
        ("c4p", xml)
    } else {
        ("xml", bytes.to_vec())
    };
    let mut report = project_xml(std::str::from_utf8(&xml).map_err(|e| e.to_string())?)?;
    report.source = Some(Source {
        format,
        sha256: sha256_hex(bytes),
        project_xml_sha256: sha256_hex(&xml),
    });
    Ok(report)
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    use std::fmt::Write;
    let mut result = String::with_capacity(64);
    for byte in Sha256::digest(bytes) {
        write!(&mut result, "{byte:02x}").expect("writing to String cannot fail");
    }
    result
}

pub mod room;
