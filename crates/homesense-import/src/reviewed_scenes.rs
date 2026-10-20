//! Explicit, fingerprint-bound scene translation. This API never enables rules,
//! publishes configuration, chooses triggers, or supplies source policy defaults.
use homesense_config::Configuration;
use homesense_core::{
    DeviceId, ModuleKind,
    rule::{Action, Node},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BindingReview {
    pub behavior: BindingBehavior,
    pub source_device: String,
    pub logical_device: String,
    pub review_evidence: String,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BindingBehavior {
    Standalone,
    ReviewedCombinedLightNormalizedLevel,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FieldReview {
    pub expected: String,
    pub disposition: Disposition,
    pub review_evidence: String,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Disposition {
    Identity,
    Display,
    Binding,
    Level,
    Delay,
    Rate,
    Toggle,
    Nonoperative,
    SourceHistory,
    Tracking,
}
#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Unit {
    Milliseconds,
    Seconds,
    Minutes,
}
impl Unit {
    fn milliseconds(self, value: u64) -> Result<u64, String> {
        value
            .checked_mul(match self {
                Self::Milliseconds => 1,
                Self::Seconds => 1000,
                Self::Minutes => 60000,
            })
            .ok_or_else(|| "source timing overflow".into())
    }
}
#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Anchor {
    SceneStart,
    PreviousElementInMember,
}
#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ZeroRate {
    Immediate,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RateBackend {
    ZeroOnly,
    SoftwareFade { interval_ms: u64, lateness_ms: u64 },
    UnsupportedHardwareRamp,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Deactivation {
    /// The exact raw toggle value must resolve in the same source Agent.
    Target { scene_id: String },
    /// Explicit review of an unresolved source value, never a numeric sentinel default.
    Unavailable { raw_value: String },
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TrackingContract {
    pub aggregation: homesense_config::scene_tracking::Aggregation,
    pub unknown_policy: homesense_config::scene_tracking::UnknownPolicy,
    pub pending_policy: homesense_config::scene_tracking::PendingPolicy,
    pub control_policy: homesense_config::scene_tracking::ControlPolicy,
    pub activity_name: String,
    pub max_age_ms: u64,
    pub review_evidence: String,
    pub members: Vec<TrackingMember>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TrackingMember {
    pub source_member_index: usize,
    pub predicate: homesense_config::scene_tracking::Predicate,
    pub review_evidence: String,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DeactivationRuntime {
    pub active_execution_policy:homesense_config::rule_document::DeactivationPolicy,
    pub review_evidence:String,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Contract {
    #[serde(default)]
    pub deactivation_runtime:Option<DeactivationRuntime>,
    pub scene: String,
    pub tracking: Option<TrackingContract>,
    pub source_sha256: String,
    pub review_evidence: String,
    pub fields: BTreeMap<String, FieldReview>,
    pub delay_unit: Unit,
    pub delay_anchor: Anchor,
    pub rate_duration_unit: Unit,
    pub rate_backend: RateBackend,
    pub zero_rate: ZeroRate,
    /// Exact source tokens explicitly reviewed as having no runtime tracking in
    /// this converted scope. Empty tokens and unsupported tracking requests hold.
    pub nonoperative_tracking_tokens: BTreeSet<String>,
    pub deactivation: Deactivation,
    pub max_lateness_ms: u64,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Review {
    pub project_xml_sha256: String,
    pub configuration_revision: u64,
    pub bindings: Vec<BindingReview>,
    pub scenes: Vec<Contract>,
}
#[derive(Debug, Serialize)]
pub struct SourceScene {
    pub id: String,
    pub agent: String,
    pub scene_id: String,
    pub display_name: String,
    pub source_sha256: String,
    pub raw_xml: String,
    pub inventory: Value,
    /// Full indexed source leaves, including fields with no runtime schema slot.
    pub fields: BTreeMap<String, String>,
    pub structural_blockers: Vec<String>,
}
#[derive(Debug, Serialize)]
pub struct Outcome {
    pub source: SourceScene,
    pub review: Option<Contract>,
    pub status: &'static str,
    pub blockers: Vec<String>,
}
#[derive(Debug, Serialize)]
pub struct Bundle {
    pub format: &'static str,
    pub automatic_execution: bool,
    pub enabled: bool,
    pub project_xml_sha256: String,
    pub configuration_revision: u64,
    pub binding_reviews: Vec<BindingReview>,
    pub scenes: Vec<Outcome>,
    pub document: Value,
}
fn evidence(value: &str) -> bool {
    !value.trim().is_empty() && value.len() <= 8192
}
fn identity(agent: &str, id: &str) -> String {
    format!("control4:scene:{agent}:{id}")
}
fn fields(
    node: roxmltree::Node<'_, '_>,
    path: &str,
    out: &mut BTreeMap<String, String>,
    blockers: &mut Vec<String>,
) {
    if node.attributes().len() != 0 || node.tag_name().namespace().is_some() {
        blockers.push(format!("unsupported source attributes at {path}"));
    }
    let mut counts = BTreeMap::<String, usize>::new();
    let children: Vec<_> = node.children().filter(|n| n.is_element()).collect();
    if children.is_empty() {
        out.insert(path.into(), node.text().unwrap_or("").trim().into());
        return;
    }
    if node
        .children()
        .any(|n| n.is_text() && n.text().is_some_and(|s| !s.trim().is_empty()))
    {
        blockers.push(format!("mixed source content at {path}"));
    }
    for child in children {
        let name = child.tag_name().name();
        let i = counts.entry(name.into()).or_default();
        fields(child, &format!("{path}/{name}[{i}]"), out, blockers);
        *i += 1;
    }
}
pub fn inventory(xml: &str) -> Result<Vec<SourceScene>, String> {
    let report = crate::project_xml(xml)?;
    let doc = crate::parse_xml(xml)?;
    let mut raw = BTreeMap::new();
    for item in doc.descendants().filter(|n| n.has_tag_name("item")) {
        let agent = crate::required(item, "id")?;
        if let Some(state) = crate::text(item, "state") {
            let Ok(state) = crate::parse_xml(state) else {
                continue;
            };
            for scene in state.descendants().filter(|n| n.has_tag_name("AdvScene")) {
                raw.insert(
                    identity(agent, crate::required(scene, "scene_id")?),
                    state.input_text()[scene.range()].to_string(),
                );
            }
        }
    }
    report
        .scenes
        .into_iter()
        .map(|s| {
            let raw_xml = raw
                .remove(&s.stable_id)
                .ok_or("source scene raw XML missing")?;
            let parsed = crate::parse_xml(&raw_xml)?;
            let mut fields_map = BTreeMap::new();
            let mut blockers = s.unhandled_fields.clone();
            fields(
                parsed.root_element(),
                "AdvScene",
                &mut fields_map,
                &mut blockers,
            );
            let parts: Vec<_> = s.stable_id.split(':').collect();
            if parts.len() != 4
                || parts[2..]
                    .iter()
                    .any(|s| s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()))
            {
                return Err("source scene Agent/ID must retain numeric identity".into());
            }
            Ok(SourceScene {
                id: s.stable_id.clone(),
                agent: parts[2].into(),
                scene_id: parts[3].into(),
                display_name: s.name.clone(),
                source_sha256: s.source_sha256.clone(),
                raw_xml,
                inventory: serde_json::to_value(s).map_err(|e| e.to_string())?,
                fields: fields_map,
                structural_blockers: blockers,
            })
        })
        .collect()
}
fn disposition(path: &str) -> Disposition {
    let name = path.rsplit('/').next().unwrap().split('[').next().unwrap();
    match name {
        "name" => Disposition::Display,
        "scene_id" => Disposition::Identity,
        "device_id" => Disposition::Binding,
        "level" => Disposition::Level,
        "delay" => Disposition::Delay,
        "rate" => Disposition::Rate,
        "toggle_id" => Disposition::Toggle,
        "is_active" => Disposition::SourceHistory,
        _ => Disposition::Nonoperative,
    }
}
fn check_fields(source: &SourceScene, review: &Contract) -> Result<(), String> {
    if !source.structural_blockers.is_empty() {
        return Err(format!(
            "unhandled source structure: {:?}",
            source.structural_blockers
        ));
    }
    if source.source_sha256 != review.source_sha256 || !evidence(&review.review_evidence) {
        return Err("scene fingerprint/review evidence mismatch".into());
    }
    if source.fields.len() != review.fields.len() {
        return Err("complete exact field dispositions required".into());
    }
    for (path, value) in &source.fields {
        let field = review
            .fields
            .get(path)
            .ok_or_else(|| format!("missing field review {path}"))?;
        if field.expected != *value
            || !evidence(&field.review_evidence)
            || field.disposition
                != if review.tracking.is_some()
                    && matches!(
                        path.rsplit('/').next().unwrap().split('[').next().unwrap(),
                        "track_mode" | "track_lvl"
                    )
                {
                    Disposition::Tracking
                } else {
                    disposition(path)
                }
        {
            return Err(format!("stale value/unsupported field disposition {path}"));
        }
        if field.disposition == Disposition::Nonoperative {
            let name = path.rsplit('/').next().unwrap().split('[').next().unwrap();
            let supported = match name {
                "track_mode" => {
                    !value.is_empty() && review.nonoperative_tracking_tokens.contains(value)
                }
                "track_lvl" => value.parse::<u8>().is_ok_and(|v| v <= 100),
                "hold_rate_up" | "hold_rate_down" => value == "0",
                "full_off" | "full_on" | "ramp_capable" | "flash" | "ignore_ramp" => {
                    matches!(value.as_str(), "False" | "false" | "0")
                }
                "top_active_color"
                | "top_inactive_color"
                | "btm_active_color"
                | "btm_inactive_color"
                | "toggle_active_color"
                | "toggle_inactive_color" => matches!(value.as_str(), "#000000" | "0"),
                _ => false,
            };
            if !supported {
                return Err(format!("runtime policy unavailable for {path}={value}"));
            }
        }
    }
    // These metadata require explicit policies, including absent source fields.
    for name in ["track_mode", "toggle_id"] {
        if !source.fields.contains_key(&format!("AdvScene/{name}[0]")) {
            return Err(format!("missing reviewed source {name}"));
        }
    }
    Ok(())
}
fn bindings(
    project: &str,
    config: &Configuration,
    reviews: &[BindingReview],
) -> Result<BTreeMap<String, crate::semantic::Binding>, String> {
    if reviews.len() > 4096 {
        return Err("binding review scope exceeds 4096".into());
    }
    let report = crate::project_xml(project)?;
    let doc = crate::parse_xml(project)?;
    let candidates = crate::semantic::candidate_bindings(&doc, &report.devices);
    let mut approved = BTreeMap::new();
    for review in reviews {
        if !evidence(&review.review_evidence) {
            return Err("binding review evidence required".into());
        }
        let original = candidates
            .get(&review.source_device)
            .ok_or("source binding identity missing")?;
        let id = DeviceId::new(&review.logical_device).map_err(|e| e.to_string())?;
        let (logical, module) = config
            .binding(&id)
            .ok_or("logical binding must be explicitly configured and enabled")?;
        let behavior = if logical.supply_relay.is_some() {
            BindingBehavior::ReviewedCombinedLightNormalizedLevel
        } else {
            BindingBehavior::Standalone
        };
        if behavior != review.behavior {
            return Err("binding behavior review must explicitly match standalone or normalized combined-light semantics".into());
        }
        if original.device != id || original.kind != module.kind() {
            return Err("source proxy must retain stable module identity/kind".into());
        }
        if approved
            .insert(
                review.source_device.clone(),
                crate::semantic::Binding {
                    device: id,
                    kind: original.kind,
                },
            )
            .is_some()
        {
            return Err("duplicate binding review".into());
        }
    }
    Ok(approved)
}
#[derive(Clone)]
struct Prepared {
    activate: Vec<Value>,
    steps: Vec<Value>,
    deactivate: Option<String>,
    timed: bool,
    tracking: Option<homesense_config::scene_tracking::Policy>,
}
fn tracking_policy(
    source: &SourceScene,
    contract: &Contract,
    bindings: &BTreeMap<String, crate::semantic::Binding>,
) -> Result<Option<homesense_config::scene_tracking::Policy>, String> {
    let Some(c) = &contract.tracking else {
        return Ok(None);
    };
    let members = source.inventory["members"]
        .as_array()
        .ok_or("source members missing")?;
    if c.members.len() != members.len() || !evidence(&c.review_evidence) {
        return Err("complete explicit source tracking member policies required".into());
    }
    let mut seen = BTreeSet::new();
    let mut translated = vec![];
    for (index, m) in c.members.iter().enumerate() {
        if index != m.source_member_index {
            return Err("tracking policies must retain source member order".into());
        }
        let member = members
            .get(m.source_member_index)
            .ok_or("tracking source member index missing")?;
        if !seen.insert(m.source_member_index) || !evidence(&m.review_evidence) {
            return Err("duplicate/unreviewed source tracking member".into());
        }
        let binding = bindings
            .get(
                member["control4_id"]
                    .as_str()
                    .ok_or("tracking source proxy missing")?,
            )
            .ok_or("tracking source binding missing")?;
        translated.push(homesense_config::scene_tracking::Member {
            id: format!("{}:member:{}", source.id, m.source_member_index),
            device: binding.device.as_str().into(),
            predicate: m.predicate.clone(),
            review_evidence: m.review_evidence.clone(),
        });
    }
    Ok(Some(homesense_config::scene_tracking::Policy {
        hap_activity: true,
        scene: source.id.clone(),
        enabled: false,
        aggregation: c.aggregation,
        unknown_policy: c.unknown_policy,
        pending_policy: c.pending_policy,
        control_policy: c.control_policy,
        activity_name: c.activity_name.clone(),
        max_age_ms: c.max_age_ms,
        review_evidence: c.review_evidence.clone(),
        members: translated,
    }))
}
fn prepare(
    source: &SourceScene,
    c: &Contract,
    bind: &BTreeMap<String, crate::semantic::Binding>,
    all: &BTreeSet<String>,
    config: &Configuration,
) -> Result<Prepared, String> {
    check_fields(source, c)?;
    let tracking = tracking_policy(source, c, bind)?;
    let raw = source
        .fields
        .get("AdvScene/toggle_id[0]")
        .ok_or("missing source toggle")?;
    let off = match &c.deactivation {
        Deactivation::Target { scene_id } => {
            let target = identity(&source.agent, scene_id);
            if raw != scene_id || !all.contains(&target) {
                return Err(
                    "toggle target must resolve from exact source Agent and raw toggle_id".into(),
                );
            }
            Some(target)
        }
        Deactivation::Unavailable { raw_value } => {
            if raw != raw_value || all.contains(&identity(&source.agent, raw)) {
                return Err("resolved source toggle cannot be discarded as unavailable".into());
            }
            None
        }
    };
    if c.deactivation_runtime.as_ref().is_some_and(|r|!evidence(&r.review_evidence)) || c.deactivation_runtime.is_some() && off.is_none() {return Err("deactivation runtime policy requires a resolved target and explicit review".into());}
    let mut activate = vec![];
    let mut steps = vec![];
    let mut timed = false;
    for (mi, member) in source.inventory["members"]
        .as_array()
        .ok_or("missing source members")?
        .iter()
        .enumerate()
    {
        let proxy = member["control4_id"]
            .as_str()
            .ok_or("source proxy missing")?;
        let binding = bind
            .get(proxy)
            .ok_or_else(|| format!("unreviewed scene member binding {proxy}"))?;
        if let Some((owner, _)) = config
            .devices()
            .iter()
            .find(|(_, d)| d.enabled && d.supply_relay.as_ref() == Some(&binding.device))
        {
            return Err(format!(
                "owned-supply/alias conflict: source member {mi} proxy {proxy} targets {} owned by logical {}; original nodes cannot be preserved",
                binding.device.as_str(),
                owner.as_str()
            ));
        }
        let mut prior = 0u64;
        for (ei, element) in member["elements"]
            .as_array()
            .ok_or("missing source elements")?
            .iter()
            .enumerate()
        {
            let delay = c
                .delay_unit
                .milliseconds(element["source_delay"].as_u64().ok_or("delay missing")?)?;
            let offset = match c.delay_anchor {
                Anchor::SceneStart => delay,
                Anchor::PreviousElementInMember => prior
                    .checked_add(delay)
                    .ok_or("cumulative delay overflow")?,
            };
            prior = offset;
            if offset > 86_400_000 {
                return Err("scene delay exceeds supported 24 hours".into());
            }
            let rate = c
                .rate_duration_unit
                .milliseconds(element["source_rate"].as_u64().ok_or("rate missing")?)?;
            let level = element["level"].as_u64().ok_or("level missing")?;
            let (logical, module) = config
                .binding(&binding.device)
                .ok_or("logical binding disabled")?;
            if module.profile() == homesense_core::ProtocolProfile::Ad8Din
                && logical.supply_relay.is_none()
                && !matches!(level, 0 | 100)
            {
                return Err(
                    "AD8 immediate intermediate level unsupported; no source-rate approximation"
                        .into(),
                );
            }
            let mut command = match binding.kind {
                ModuleKind::Switch if matches!(level, 0 | 100) => {
                    json!({"kind":"switch","on":level!=0})
                }
                ModuleKind::Dimmer => json!({"kind":"level","value":level,"ramp":false}),
                _ => return Err("reviewed scene level/device kind not representable".into()),
            };
            let fade = if rate == 0 {
                match c.zero_rate {
                    ZeroRate::Immediate => None,
                }
            } else {
                match c.rate_backend {
                    RateBackend::SoftwareFade {
                        interval_ms,
                        lateness_ms,
                    } if binding.kind == ModuleKind::Dimmer => {
                        if config.binding(&binding.device).is_some_and(|(_, m)| {
                            m.profile() == homesense_core::ProtocolProfile::Ad8Din
                        }) {
                            return Err("AD8 software fade requires unsupported immediate intermediate levels".into());
                        }
                        let timing = homesense_config::rule_document::FadeTiming {
                            duration_ms: rate,
                            interval_ms,
                            lateness_ms,
                        };
                        timing.samples().map_err(|e| e.to_string())?;
                        command["ramp"] = false.into();
                        Some(
                            json!({"duration_ms":rate,"interval_ms":interval_ms,"lateness_ms":lateness_ms}),
                        )
                    }
                    _ => {
                        return Err(
                            "source rate requires an explicit supported duration/backend".into(),
                        );
                    }
                }
            };
            let source_label = format!(
                "{}/member:{mi}/element:{ei}/sha256:{}",
                source.id, source.source_sha256
            );
            let mut step = json!({"source":source_label,"offset_ms":offset,"device":binding.device.as_str(),"command":command});
            if let Some(fade) = fade {
                step["fade"] = fade;
                timed = true;
            }
            timed |= offset != 0;
            activate.push(json!({"source":source_label,"action":{"kind":"command","device":binding.device.as_str(),"command":command}}));
            steps.push(step);
        }
    }
    if activate.is_empty() {
        return Err("empty source scene cannot be converted".into());
    }
    Ok(Prepared {
        activate,
        steps,
        deactivate: off,
        timed,
        tracking,
    })
}
fn deactivation_link(id:&str,target:&str,c:&Contract)->Value {
    let r=c.deactivation_runtime.as_ref().unwrap();
    json!({"scene":id,"target":target,"enabled":false,"review_evidence":r.review_evidence,"active_execution_policy":r.active_execution_policy})
}
fn runtime_definition(
    id: &str,
    p: &Prepared,
    c: &Contract,
    prepared: &BTreeMap<String, Prepared>,
) -> Value {
    if p.timed {
        json!({"id":id,"enabled":false,"review_evidence":c.review_evidence,"max_lateness_ms":c.max_lateness_ms,"steps":p.steps})
    } else {
        let off = if c.deactivation_runtime.is_some() {None} else {p
            .deactivate
            .as_ref()
            .and_then(|id| prepared.get(id))
            .map(|q| q.activate.clone())};
        json!({"id":id,"enabled":false,"review_evidence":c.review_evidence,"activate":p.activate,"deactivate":off})
    }
}
/// Build a complete strict document with no enabled scene or rule. Unsupported
/// fields hold the whole scene; dependency failures hold each dependent scene.
pub fn convert(project: &str, config: &Configuration, review: &Review) -> Result<Bundle, String> {
    let fingerprint = crate::sha256_hex(project.as_bytes());
    if review.project_xml_sha256 != fingerprint
        || review.configuration_revision != config.revision()
    {
        return Err("project/configuration review scope mismatch".into());
    }
    let bind = bindings(project, config, &review.bindings)?;
    let sources = inventory(project)?;
    if review.scenes.len() > 1024 {
        return Err("scene contract budget exceeded".into());
    }
    let mut contracts = BTreeMap::new();
    let all: BTreeSet<_> = sources.iter().map(|s| s.id.clone()).collect();
    for c in &review.scenes {
        if !all.contains(&c.scene) || contracts.insert(c.scene.clone(), c).is_some() {
            return Err("unknown/duplicate reviewed scene".into());
        }
    }
    let mut prepared = BTreeMap::new();
    let mut held = BTreeMap::new();
    for source in &sources {
        match contracts
            .get(&source.id)
            .ok_or_else(|| "explicit scene contract missing".to_string())
            .and_then(|c| prepare(source, c, &bind, &all, config))
        {
            Ok(p) => {
                prepared.insert(source.id.clone(), p);
            }
            Err(e) => {
                held.insert(source.id.clone(), e);
            }
        }
    }
    // Dependencies are deliberately one target activation vector, never the
    // source members, source-off inversion, union, or target's own deactivation.
    loop {
        let mut remove = vec![];
        for (id, p) in &prepared {
            if let Some(target) = &p.deactivate {
                match prepared.get(target) {
                    None => remove.push((
                        id.clone(),
                        format!("deactivation dependency held: {target}"),
                    )),
                    Some(q) if (p.timed || q.timed) && contracts[id].deactivation_runtime.is_none() => remove.push((
                        id.clone(),
                        "deferred deactivation requires explicit quiescent runtime policy; whole scene held".into(),
                    )),
                    _ => {}
                }
            }
            let definition = runtime_definition(id, p, contracts[id], &prepared);
            let mut probe = json!({"schema_version":1,"configuration_revision":config.revision(),"variables":[],"rules":[],"immediate_scenes":[],"timed_scenes":[],"scene_tracking":p.tracking.iter().collect::<Vec<_>>(),"scene_metadata":[{"id":id,"name":sources.iter().find(|s|s.id==*id).ok_or("source metadata missing")?.display_name}]});
            probe[if p.timed {
                "timed_scenes"
            } else {
                "immediate_scenes"
            }] = json!([definition]);
            if contracts[id].deactivation_runtime.is_some() && let Some(target)=&p.deactivate && let Some(q)=prepared.get(target) {
                let key=if q.timed {"timed_scenes"} else {"immediate_scenes"};
                probe[key].as_array_mut().unwrap().push(runtime_definition(target,q,contracts[target],&prepared));
                probe["scene_deactivations"]=json!([deactivation_link(id,target,contracts[id])]);
            }
            if let Err(error) = homesense_config::rule_document::Document::parse(
                &serde_json::to_vec(&probe).map_err(|e| e.to_string())?,
                config,
            ) {
                remove.push((
                    id.clone(),
                    format!("strict scene runtime unsupported: {error}"),
                ));
            }
        }
        if remove.is_empty() {
            break;
        }
        for (id, reason) in remove {
            prepared.remove(&id);
            held.insert(id, reason);
        }
    }
    let mut immediate = vec![];
    let mut timed = vec![];
    for (id, p) in &prepared {
        let definition = runtime_definition(id, p, contracts[id], &prepared);
        if p.timed {
            timed.push(definition);
        } else {
            immediate.push(definition);
        }
    }
    let mut document = json!({"schema_version":1,"configuration_revision":config.revision(),"variables":[],"rules":[],"immediate_scenes":immediate,"timed_scenes":timed,"scene_tracking":prepared.values().filter_map(|p|p.tracking.as_ref()).collect::<Vec<_>>(),"scene_metadata":sources.iter().filter(|s|prepared.contains_key(&s.id)).map(|s|json!({"id":s.id,"name":s.display_name})).collect::<Vec<_>>()});
    let links=prepared.iter().filter_map(|(id,p)|p.deactivate.as_ref().filter(|_|contracts[id].deactivation_runtime.is_some()).map(|target|deactivation_link(id,target,contracts[id]))).collect::<Vec<_>>();
    if !links.is_empty(){document["scene_deactivations"]=json!(links);}
    // Do not export definitions which the actual strict runtime schema rejects.
    homesense_config::rule_document::Document::parse(
        &serde_json::to_vec(&document).map_err(|e| e.to_string())?,
        config,
    )
    .map_err(|e| format!("strict scene document: {e}"))?;
    let scenes = sources
        .into_iter()
        .map(|source| {
            let review = contracts.get(&source.id).map(|c| (*c).clone());
            let blockers = held.remove(&source.id).into_iter().collect::<Vec<_>>();
            Outcome {
                source,
                review,
                status: if blockers.is_empty() {
                    "converted_disabled"
                } else {
                    "held"
                },
                blockers,
            }
        })
        .collect();
    Ok(Bundle {
        format: "homesense-reviewed-scene-draft-v1",
        automatic_execution: false,
        enabled: false,
        project_xml_sha256: fingerprint,
        configuration_revision: config.revision(),
        binding_reviews: review.bindings.clone(),
        scenes,
        document,
    })
}
/// Resolve each source call by the *action target Agent*, not the calling event
/// device or owneriditem alone. Exact archived event XML is required. Variables,
/// trigger and lifetime reviews remain separate; all outer control flow survives.
pub fn link_event(
    project: &str,
    event_xml: &str,
    config: &Configuration,
    review: &Review,
    bundle: &Bundle,
) -> Result<Value, String> {
    if bundle.project_xml_sha256 != crate::sha256_hex(project.as_bytes())
        || bundle.configuration_revision != config.revision()
    {
        return Err("linked bundle source/configuration mismatch".into());
    }
    // Recompute rather than trusting a caller-mutated public Bundle.document.
    let authoritative = convert(project, config, review)?;
    if authoritative.document != bundle.document {
        return Err("linked scene document was modified after review".into());
    }
    let event = crate::parse_xml(event_xml)?;
    let root = event.root_element();
    let device = crate::required(root, "deviceid")?;
    let event_id = crate::required(root, "eventid")?;
    let doc = crate::parse_xml(project)?;
    let events: Vec<_> = doc
        .root_element()
        .children()
        .filter(|n| n.has_tag_name("event_mgr"))
        .flat_map(|m| m.children().filter(|n| n.has_tag_name("event")))
        .filter(|n| {
            crate::text(*n, "deviceid") == Some(device)
                && crate::text(*n, "eventid") == Some(event_id)
        })
        .collect();
    if events.len() != 1 || &project[events[0].range()] != event_xml {
        return Err("event must match one exact archived project event".into());
    }
    let bind = bindings(project, config, &review.bindings)?;
    let mut compiled = crate::semantic::compile_event(event_xml, &bind)?;
    let mut calls = BTreeMap::new();
    for code in root.descendants().filter(|n| n.has_tag_name("codeitem")) {
        let Some(command) = code
            .children()
            .find(|n| n.has_tag_name("cmdcond"))
            .and_then(|n| n.children().find(|n| n.has_tag_name("devicecommand")))
        else {
            continue;
        };
        if !matches!(
            crate::text(command, "command"),
            Some("ACTIVATE_SCENE" | "DEACTIVATE_SCENE")
        ) {
            continue;
        }
        let agent = crate::required(code, "device")?;
        let raw = crate::semantic::parameter(command, "SCENE_ID")?;
        let mut chain: Vec<_> = code
            .ancestors()
            .filter(|n| n.has_tag_name("codeitem"))
            .collect();
        chain.reverse();
        let mut key = format!("device:{device}/event:{event_id}");
        for c in chain {
            key.push_str(&format!("/code:{}", crate::required(c, "id")?));
        }
        calls.insert(key, identity(agent, &raw));
    }
    fn rewrite(
        nodes: &mut [Node],
        calls: &BTreeMap<String, String>,
        bundle: &Bundle,
    ) -> Result<(), String> {
        for node in nodes {
            match &mut node.action {
                Action::Scene { id, activate } => {
                    let qualified = calls
                        .get(&node.source)
                        .ok_or("qualified scene call provenance missing")?;
                    let definition = bundle.document["immediate_scenes"]
                        .as_array()
                        .and_then(|scenes| scenes.iter().find(|s| s["id"] == *qualified));
                    // Timed activation is allowed only with the explicit deferred adapter.
                    let timed = bundle.document["timed_scenes"]
                        .as_array()
                        .is_some_and(|scenes| scenes.iter().any(|s| s["id"] == *qualified));
                    let linked_off=bundle.document["scene_deactivations"].as_array().is_some_and(|links|links.iter().any(|l|l["scene"]==*qualified));
                    if definition.is_none() && !(timed && (*activate || linked_off)) {
                        return Err(format!(
                            "qualified source scene held or unresolved: {qualified}"
                        ));
                    }
                    if !*activate && !linked_off && definition.is_none_or(|s| s["deactivate"].is_null()) {
                        return Err("source deactivation unavailable".into());
                    }
                    *id = qualified.clone();
                }
                Action::If {
                    then_actions,
                    else_actions,
                    ..
                } => {
                    rewrite(then_actions, calls, bundle)?;
                    rewrite(else_actions, calls, bundle)?;
                }
                _ => {}
            }
        }
        Ok(())
    }
    rewrite(&mut compiled, &calls, bundle)?;
    let event = crate::semantic::CompiledEvent {
        actions: compiled,
        timed_scenes: Default::default(),
    };
    let mut draft = crate::event_export::draft(&event)?;
    draft["project_xml_sha256"] = bundle.project_xml_sha256.clone().into();
    draft["event_xml_sha256"] = crate::sha256_hex(event_xml.as_bytes()).into();
    draft["source_event_xml"] = event_xml.into();
    draft["scene_call_links"] = serde_json::to_value(calls).map_err(|e| e.to_string())?;
    draft["source_key"] = format!("{device}:{event_id}").into();
    draft["qualified_scene_document"] = bundle.document.clone();
    Ok(draft)
}
