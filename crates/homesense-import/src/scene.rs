//! Preserve disabled lighting scenes before translating timing or tracking rules.
use roxmltree::Node;
use serde::Serialize;
use std::collections::BTreeMap;
#[derive(Debug, Serialize)]
pub struct Scene {
    pub stable_id: String,
    pub name: String,
    pub enabled: bool,
    pub metadata: BTreeMap<String, String>,
    pub members: Vec<Member>,
    pub unhandled_fields: Vec<String>,
    pub source_sha256: String,
}
#[derive(Debug, Serialize)]
pub struct Member {
    pub control4_id: String,
    pub metadata: BTreeMap<String, String>,
    pub elements: Vec<Element>,
}
#[derive(Debug, Serialize)]
pub struct Element {
    pub level: u8,
    pub source_delay: u64,
    pub source_rate: u64,
}
fn metadata(
    node: Node<'_, '_>,
    allowed: &[&str],
    structural: &[&str],
    unhandled: &mut Vec<String>,
) -> Result<BTreeMap<String, String>, String> {
    let mut result = BTreeMap::new();
    let mut seen = std::collections::BTreeSet::new();
    for child in node.children().filter(|n| n.is_element()) {
        let name = child.tag_name().name();
        if !seen.insert(name) {
            return Err(format!("duplicate scene field {name}"));
        }
        if structural.contains(&name) {
            continue;
        }
        if allowed.contains(&name) && !child.children().any(|n| n.is_element()) {
            result.insert(name.into(), child.text().unwrap_or("").trim().into());
        } else {
            unhandled.push(name.into());
        }
    }
    Ok(result)
}
pub(crate) fn parse(node: Node<'_, '_>, owner: &str) -> Result<Scene, String> {
    let mut unhandled_fields = Vec::new();
    let metadata = metadata(
        node,
        &[
            "track_mode",
            "top_active_color",
            "top_inactive_color",
            "btm_active_color",
            "btm_inactive_color",
            "toggle_active_color",
            "toggle_inactive_color",
            "hold_rate_up",
            "hold_rate_down",
            "toggle_id",
            "is_active",
            "full_off",
            "full_on",
            "ramp_capable",
        ],
        &["name", "scene_id", "all_members"],
        &mut unhandled_fields,
    )?;
    let mut members = Vec::new();
    if let Some(all) = node.children().find(|n| n.has_tag_name("all_members")) {
        for member in all.children().filter(|n| n.is_element()) {
            if !member.has_tag_name("AdvSceneMember") {
                unhandled_fields.push(format!("all_members/{}", member.tag_name().name()));
                continue;
            }
            let mut elements = Vec::new();
            let member_metadata = self::metadata(
                member,
                &["track_mode", "track_lvl", "flash", "ignore_ramp"],
                &["device_id", "all_elements"],
                &mut unhandled_fields,
            )?;
            if let Some(all) = member.children().find(|n| n.has_tag_name("all_elements")) {
                for element in all.children().filter(|n| n.is_element()) {
                    if !element.has_tag_name("element") {
                        unhandled_fields
                            .push(format!("all_elements/{}", element.tag_name().name()));
                        continue;
                    }
                    self::metadata(
                        element,
                        &["delay", "rate", "level"],
                        &[],
                        &mut unhandled_fields,
                    )?;
                    let number = |key| {
                        super::required(element, key)?
                            .parse::<u64>()
                            .map_err(|_| format!("invalid scene {key}"))
                    };
                    let level = number("level")?;
                    if level > 100 {
                        return Err("scene level outside 0..100".into());
                    }
                    elements.push(Element {
                        level: level as u8,
                        source_delay: number("delay")?,
                        source_rate: number("rate")?,
                    });
                }
            }
            if elements.is_empty() {
                unhandled_fields.push("member_without_elements".into());
            }
            members.push(Member {
                control4_id: super::required(member, "device_id")?.into(),
                metadata: member_metadata,
                elements,
            });
        }
    }
    if members.is_empty() {
        unhandled_fields.push("scene_without_members".into());
    }
    Ok(Scene {
        stable_id: format!(
            "control4:scene:{owner}:{}",
            super::required(node, "scene_id")?
        ),
        name: super::required(node, "name")?.into(),
        enabled: false,
        metadata,
        members,
        unhandled_fields,
        source_sha256: super::sha256_hex(&node.document().input_text().as_bytes()[node.range()]),
    })
}
