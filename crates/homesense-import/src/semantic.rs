//! Conservative source XML -> typed rule AST conversion. Bindings are supplied
//! by reviewed configuration; this does not enable imported devices or rules.
use homesense_core::{
    Command, DeviceId, LevelTransition, ModuleKind, Percent,
    rule::{Action, Condition, Expr, Node, Value},
};
use roxmltree::Node as Xml;
use std::collections::BTreeMap;
pub struct Binding {
    pub device: DeviceId,
    pub kind: ModuleKind,
}
/// Mapping approval is scoped to one exact source event and target. It does
/// not approve the source guard's evidence, trigger or variable lifecycle.
pub struct MotorReview {
    pub source_key:String,
    pub source_xml_sha256:String,
    pub source_target:String,
    pub device:DeviceId,
    pub mapping_review:String,
}
#[derive(Clone)]
pub struct MotorOptions<'a> {
    configuration:&'a homesense_config::Configuration,
    reviews:&'a [MotorReview],
    canonical:std::sync::Arc<BTreeMap<String,DeviceId>>,
    events:std::sync::Arc<BTreeMap<String,String>>,
}
impl<'a> MotorOptions<'a> {
    /// The source project establishes parent/proxy identity only. Configuration
    /// and explicit event-scoped mapping review must independently calibrate it.
    pub fn reviewed_project(configuration:&'a homesense_config::Configuration,project_xml:&str,reviews:&'a [MotorReview])->Result<Self,String> {
        if reviews.len()>256{return Err("motor review scope exceeds 256".into());}
        let report=crate::project_xml(project_xml)?;let doc=crate::parse_xml(project_xml)?;
        let mut canonical=BTreeMap::new();
        for device in report.devices.iter().filter(|d|d.kind=="motor") {
            let controller=doc.descendants().find(|n|n.has_tag_name("item")&&crate::text(*n,"id")==Some(device.control4_id.as_str())).ok_or("source motor controller missing")?;
            if crate::text(controller,"type")!=Some("6"){return Err("source motor controller must retain type6 identity".into());}
            let id=DeviceId::new(device.stable_id.clone()).map_err(|e|e.to_string())?;canonical.insert(device.control4_id.clone(),id.clone());
            let proxies=controller.descendants().filter(|n|n.has_tag_name("item")&&crate::text(*n,"type")==Some("7")
                && n.ancestors().skip(1).find(|p|p.has_tag_name("item"))==Some(controller))
                .filter(|n|n.children().find(|c|c.has_tag_name("itemdata")).and_then(|c|crate::text(c,"config_data_file"))==Some("blind.c4i")).collect::<Vec<_>>();
            if proxies.len()!=1{return Err("source motor needs one unambiguous type7 blind proxy".into());}
            canonical.insert(text(proxies[0],"id")?.into(),id);
        }
        let mut events=BTreeMap::new();
        for event in doc.root_element().children().find(|n|n.has_tag_name("event_mgr")).into_iter().flat_map(|m|m.children().filter(|n|n.has_tag_name("event"))) {
            let key=format!("{}:{}",text(event,"deviceid")?,text(event,"eventid")?);
            if events.insert(key,xml_sha256(&project_xml[event.range()])).is_some(){return Err("duplicate motor source event scope".into());}
        }
        for review in reviews {
            if canonical.get(&review.source_target)!=Some(&review.device) || events.get(&review.source_key)!=Some(&review.source_xml_sha256) {
                return Err("motor proxy/parent identity or archived event scope mismatch".into());
            }
            configuration.motor_command(&review.device,homesense_core::MotorAction::Stop).map_err(|e|format!("motor calibration required: {e}"))?;
        }
        Ok(Self{configuration,reviews,canonical:std::sync::Arc::new(canonical),events:std::sync::Arc::new(events)})
    }
}
struct MotorContext<'a> {options:MotorOptions<'a>,source_key:String,sha256:String}
impl MotorContext<'_> {
    fn binding<'a>(&'a self,target:&str,bindings:&BTreeMap<String,Binding>)->Result<&'a MotorReview,String> {
        let mut matches=self.options.reviews.iter().filter(|r|r.source_key==self.source_key && r.source_target==target);
        let review=matches.next().ok_or_else(||format!("unreviewed motor scope {target}"))?;
        if matches.next().is_some() || review.source_xml_sha256!=self.sha256 || review.mapping_review.trim().is_empty() || review.mapping_review.len()>8192 {
            return Err("motor mapping review source hash/scope mismatch".into());
        }
        let binding=bindings.get(target).ok_or("motor source binding missing")?;
        if binding.kind!=ModuleKind::Motor || binding.device!=review.device || self.options.canonical.get(target)!=Some(&review.device)
            || self.options.events.get(&self.source_key)!=Some(&self.sha256) {
            return Err("motor proxy must retain its reviewed stable logical identity".into());
        }
        self.options.configuration.motor_command(&review.device,homesense_core::MotorAction::Stop).map_err(|e|format!("motor calibration required: {e}"))?;
        Ok(review)
    }
}
fn xml_sha256(xml:&str)->String {crate::sha256_hex(xml.as_bytes())}
fn child<'a, 'i>(n: Xml<'a, 'i>, tag: &str) -> Result<Xml<'a, 'i>, String> {
    let mut matches = n.children().filter(|v| v.has_tag_name(tag));
    let first = matches.next().ok_or_else(|| format!("missing {tag}"))?;
    if matches.next().is_some() {
        return Err(format!("duplicate {tag}"));
    }
    Ok(first)
}
fn text<'a, 'i>(n: Xml<'a, 'i>, tag: &str) -> Result<&'a str, String> {
    child(n, tag)?.text().ok_or_else(|| format!("empty {tag}"))
}
fn scalar(n: Xml<'_, '_>) -> Result<Expr, String> {
    let p = child(n, "param")?;
    if p.attribute("name") != Some("value") || p.attribute("type") != Some("int") {
        return Err("unsupported variable operand".into());
    }
    Ok(Expr::Literal(Value::Integer(
        p.text()
            .ok_or("missing integer")?
            .parse()
            .map_err(|_| "invalid integer")?,
    )))
}
fn parameter(n: Xml<'_, '_>, name: &str) -> Result<String, String> {
    let params = child(n, "params")?;
    let ps: Vec<_> = params.children().filter(|p| p.is_element()).collect();
    if ps.len() != 1 || text(ps[0], "name")? != name {
        return Err("unsupported command parameters".into());
    }
    let value = child(ps[0], "value")?;
    if value.children().filter(|n| n.is_element()).count() != 1 {
        return Err("dynamic parameter unsupported".into());
    }
    Ok(text(value, "static")?.to_owned())
}
pub(crate) fn condition(
    n: Xml<'_, '_>,
    target: &str,
    bindings: &BTreeMap<String, Binding>,
) -> Result<Condition, String> {
    condition_with_motor(n,target,bindings,None)
}
fn condition_with_motor(n:Xml<'_, '_>,target:&str,bindings:&BTreeMap<String,Binding>,motors:Option<&MotorContext<'_>>)->Result<Condition,String> {
    if n.attribute("owneridtype") != Some("variable") {
        let name = text(n, "name")?;
        let binding = bindings
            .get(target)
            .ok_or_else(|| format!("unreviewed condition target {target}"))?;
        if binding.kind==ModuleKind::Motor && matches!(name,"IF_STOPPED"|"IF_MOVING")
            && let Some(motors)=motors {
                if n.children().filter(|n|n.is_element()).count()!=1 || n.attribute("owneridtype")!=Some("") || n.attribute("owneriditem")!=Some("-1") {return Err("invalid motor condition shape".into());}
                let review=motors.binding(target,bindings)?;
                return Ok(Condition::MotorState(homesense_core::rule_motor::Predicate {device:review.device.clone(),state:if name=="IF_STOPPED"{homesense_core::rule_motor::State::Stopped}else{homesense_core::rule_motor::State::Moving},
                    evidence:homesense_core::rule_motor::Evidence::ActualOnly,max_age_ms:1000,mapping_review:review.mapping_review.clone(),evidence_review:None}));
        }
        if name == "IS_LEVEL" && binding.kind == ModuleKind::Dimmer {
            if n.children().filter(|v| v.is_element()).count() != 3
                || text(n, "conditional")? != "GREATER_THAN"
            {
                return Err("unsupported level comparison".into());
            }
            let threshold = Percent::new(
                parameter(n, "SETTING")?
                    .parse()
                    .map_err(|_| "invalid level threshold")?,
            )
            .map_err(|e| e.to_string())?;
            return Ok(Condition::Less(
                Expr::Literal(Value::Integer(i64::from(threshold.get()))),
                Expr::DeviceLevel(binding.device.clone()),
            ));
        }
        if !matches!(binding.kind, ModuleKind::Switch | ModuleKind::Dimmer)
            || !matches!(name, "LOAD_ON" | "LOAD_OFF")
            || n.children().filter(|v| v.is_element()).count() != 1
        {
            return Err(format!("unsupported device-state condition {name}"));
        }
        return Ok(Condition::Equal(
            Expr::DeviceOn(binding.device.clone()),
            Expr::Literal(Value::Bool(name == "LOAD_ON")),
        ));
    }
    let variable = Expr::Variable(
        n.attribute("owneriditem")
            .ok_or("missing variable id")?
            .into(),
    );
    let value = scalar(n)?;
    Ok(match n.attribute("name") {
        Some("==") => Condition::Equal(variable, value),
        Some("!=") => Condition::Not(Box::new(Condition::Equal(variable, value))),
        Some("<") => Condition::Less(variable, value),
        _ => return Err("unsupported condition operator".into()),
    })
}
// The project encodes one binary connective and one additional condition.
// Reject wider chains until their precedence is explicitly represented.
fn compound_condition(
    code: Xml<'_, '_>,
    bindings: &BTreeMap<String, Binding>,
    depth: usize,
    motors:Option<&MotorContext<'_>>,
) -> Result<Condition, String> {
    if depth > 64 {
        return Err("condition nesting exceeds 64".into());
    }
    let base = condition_with_motor(
        child(child(code, "cmdcond")?, "deviceconditional")?,
        text(code, "device")?,
        bindings,
        motors,
    )?;
    if !code.children().any(|n| n.has_tag_name("expression")) {
        return Ok(base);
    }
    let expression = child(code, "expression")?;
    let parts: Vec<_> = expression.children().filter(|n| n.is_element()).collect();
    if parts.len() != 2 || parts.iter().any(|n| !n.has_tag_name("codeitem")) {
        return Err("unsupported compound condition shape".into());
    }
    for part in &parts {
        if text(*part, "enabled")? != "True"
            || child(*part, "subitems")?.children().any(|n| n.is_element())
        {
            return Err("disabled or executable expression operand".into());
        }
    }
    let op = parts[0];
    if text(op, "type")? != "6"
        || text(parts[1], "type")? != "2"
        || child(op, "cmdcond")?.children().any(|n| n.is_element())
        || op.children().any(|n| n.has_tag_name("expression"))
    {
        return Err("invalid compound operator".into());
    }
    let rhs = compound_condition(parts[1], bindings, depth + 1, motors)?;
    match text(op, "display")? {
        "AND" => Ok(Condition::All(vec![base, rhs])),
        "OR" => Ok(Condition::Any(vec![base, rhs])),
        _ => Err("unknown compound operator".into()),
    }
}
pub(crate) fn action(
    n: Xml<'_, '_>,
    target: &str,
    bindings: &BTreeMap<String, Binding>,
) -> Result<Action, String> {
    action_with_motor(n,target,bindings,None)
}
fn action_with_motor(n:Xml<'_, '_>,target:&str,bindings:&BTreeMap<String,Binding>,motors:Option<&MotorContext<'_>>)->Result<Action,String> {
    if n.attribute("owneridtype") == Some("variable") {
        let name = n
            .attribute("owneriditem")
            .ok_or("missing variable id")?
            .to_owned();
        let operand = scalar(n)?;
        let value = match n.attribute("name") {
            Some("=") => operand,
            Some("+") => Expr::Add(Box::new(Expr::Variable(name.clone())), Box::new(operand)),
            Some("-") => Expr::Subtract(Box::new(Expr::Variable(name.clone())), Box::new(operand)),
            _ => return Err("unsupported variable operation".into()),
        };
        return Ok(Action::Set {
            variable: name,
            value,
        });
    }
    let command = text(n, "command")?;
    if matches!(command, "ACTIVATE_SCENE" | "DEACTIVATE_SCENE") {
        let id = parameter(n, "SCENE_ID")?;
        if n.attribute("owneridtype") != Some("agent")
            || n.attribute("owneriditem") != Some(id.as_str())
        {
            return Err("scene reference mismatch".into());
        }
        return Ok(Action::Scene {
            id,
            activate: command == "ACTIVATE_SCENE",
        });
    }
    let binding = bindings
        .get(target)
        .ok_or_else(|| format!("unreviewed target {target}"))?;
    if binding.kind==ModuleKind::Motor && matches!(command,"SET_LEVEL_TARGET"|"STOP")
        && let Some(motors)=motors {
            if n.children().filter(|n|n.is_element()).count()!=2 || n.attribute("owneridtype")!=Some("") || n.attribute("owneriditem")!=Some("-1") {return Err("invalid motor action shape".into());}
            let review=motors.binding(target,bindings)?;
            let action=if command=="STOP" {
                if child(n,"params")?.children().any(|n|n.is_element()) {return Err("unexpected motor STOP parameters".into());}
                homesense_core::MotorAction::Stop
            } else {
                let param=child(child(n,"params")?,"param")?;
                let value=child(param,"value")?;
                if param.children().filter(|n|n.is_element()).count()!=2 || !matches!(value.attribute("type"),Some("STRING"|"INTEGER")) {return Err("motor target requires a static typed LEVEL_TARGET_NAME enum".into());}
                match parameter(n,"LEVEL_TARGET_NAME")?.as_str() {"Open"=>homesense_core::MotorAction::Open,"Closed"=>homesense_core::MotorAction::Close,_=>return Err("motor target accepts only exact Open or Closed".into())}
            };
            return Ok(Action::Motor{device:review.device.clone(),action,mapping_review:review.mapping_review.clone()});
    }
    if binding.kind == ModuleKind::Switch && command == "TOGGLE" {
        if child(n, "params")?.children().any(|p| p.is_element()) {
            return Err("unexpected toggle parameters".into());
        }
        let branch = |on| Node {
            source: format!("target:{target}/TOGGLE/{on}"),
            action: Action::Command {
                device: binding.device.clone(),
                command: Command::Switch(on),
            },
        };
        return Ok(Action::If {
            condition: Condition::Equal(
                Expr::DeviceOn(binding.device.clone()),
                Expr::Literal(Value::Bool(true)),
            ),
            then_actions: vec![branch(false)],
            else_actions: vec![branch(true)],
        });
    }
    let (device, command) = match (binding.kind, command) {
        (ModuleKind::Keypad, "Turn On" | "Turn Off") => {
            let key: u8 = parameter(n, "LED Number")?
                .parse()
                .map_err(|_| "invalid LED key")?;
            if !(1..=6).contains(&key) {
                return Err("LED key outside reviewed six-key profile".into());
            }
            (
                DeviceId::new(format!("{}:key:{key}", binding.device.as_str()))
                    .map_err(|e| e.to_string())?,
                Command::PanelLed(command == "Turn On"),
            )
        }
        (ModuleKind::Switch, "ON" | "OFF") => {
            if child(n, "params")?.children().any(|p| p.is_element()) {
                return Err("unexpected switch parameters".into());
            }
            (binding.device.clone(), Command::Switch(command == "ON"))
        }
        (ModuleKind::Dimmer, "OFF") => {
            if child(n, "params")?.children().any(|p| p.is_element()) {
                return Err("unexpected dimmer OFF parameters".into());
            }
            (
                binding.device.clone(),
                Command::SetLevel {
                    level: Percent::new(0).unwrap(),
                    transition: LevelTransition::Immediate,
                },
            )
        }
        (ModuleKind::Dimmer, "SET_LEVEL") => {
            let level = Percent::new(
                parameter(n, "LEVEL")?
                    .parse()
                    .map_err(|_| "invalid level")?,
            )
            .map_err(|e| e.to_string())?;
            (
                binding.device.clone(),
                Command::SetLevel {
                    level,
                    transition: LevelTransition::Immediate,
                },
            )
        }
        _ => return Err(format!("unsupported typed command {command}")),
    };
    Ok(Action::Command { device, command })
}
/// Generated definitions remain disabled until device and source semantics review.
pub struct CompiledEvent {
    pub actions: Vec<Node>,
    pub timed_scenes: BTreeMap<String, serde_json::Value>,
}
struct FadeContext {
    definitions: BTreeMap<String, serde_json::Value>,
    interval_ms: u64,
    lateness_ms: u64,
}
/// Isolated typed-site audit; uses the same fade converter as whole-event export.
pub(crate) fn compile_fade_site(
    payload: Xml<'_, '_>,
    target: &str,
    bindings: &BTreeMap<String, Binding>,
    interval_ms: u64,
    lateness_ms: u64,
) -> Result<CompiledEvent, String> {
    let mut context = FadeContext { definitions: Default::default(), interval_ms, lateness_ms };
    let action = timed_fade(payload, target, bindings, "source-review-site", &mut context)?;
    Ok(CompiledEvent { actions: vec![Node { source: "source-review-site".into(), action }], timed_scenes: context.definitions })
}
fn timed_fade(n:Xml<'_, '_>,target:&str,bindings:&BTreeMap<String,Binding>,source:&str,context:&mut FadeContext)->Result<Action,String> {
    let binding=bindings.get(target).ok_or("unreviewed fade target")?;
    if binding.kind!=ModuleKind::Dimmer || n.children().filter(|n|n.is_element()).count()!=2 {return Err("invalid fade command shape or target".into());}
    let mut values=BTreeMap::new();
    for param in child(n,"params")?.children().filter(|n|n.is_element()) {
        if !param.has_tag_name("param") || param.children().filter(|n|n.is_element()).count()!=2 {return Err("invalid fade parameter".into());}
        let name=text(param,"name")?;
        let value=child(param,"value")?;
        if !matches!(name,"TIME"|"LEVEL") || value.attribute("type")!=Some("INTEGER")
            || value.children().filter(|n|n.is_element()).count()!=1 {return Err("unsupported fade operand".into());}
        let number=text(value,"static")?.parse::<u64>().map_err(|_|"invalid fade integer")?;
        if values.insert(name,number).is_some() {return Err("duplicate fade parameter".into());}
    }
    if values.len()!=2 {return Err("fade requires TIME and LEVEL".into());}
    let level=Percent::new(u8::try_from(values["LEVEL"]).map_err(|_|"invalid fade level")?).map_err(|e|e.to_string())?;
    homesense_config::rule_document::FadeTiming{duration_ms:values["TIME"],interval_ms:context.interval_ms,lateness_ms:context.lateness_ms}.samples().map_err(|e|e.to_string())?;
    let id=format!("imported-fade:{source}");
    if id.len()>256 {return Err("generated fade identity too long".into());}
    let definition=serde_json::json!({"id":id,"enabled":false,"max_lateness_ms":1000,"steps":[{
        "source":source,"offset_ms":0,"device":binding.device.as_str(),"command":{"kind":"level","value":level.get(),"ramp":false},
        "fade":{"duration_ms":values["TIME"],"interval_ms":context.interval_ms,"lateness_ms":context.lateness_ms}
    }]});
    if context.definitions.insert(id.clone(),definition).is_some() {return Err("duplicate generated fade".into());}
    Ok(Action::Scene{id,activate:true})
}
fn sequence(
    parent: Xml<'_, '_>,
    bindings: &BTreeMap<String, Binding>,
    prefix: &str,
    depth: usize,
    fades: &mut Option<FadeContext>,
    motors:Option<&MotorContext<'_>>,
) -> Result<Vec<Node>, String> {
    if depth > 64 {
        return Err("program nesting exceeds 64".into());
    }
    let mut result: Vec<Node> = vec![];
    let mut else_seen = false;
    if parent.has_tag_name("subitems")
        && parent
            .children()
            .any(|n| n.is_element() && !n.has_tag_name("codeitem"))
    {
        return Err("unknown executable container".into());
    }
    for code in parent.children().filter(|n| n.has_tag_name("codeitem")) {
        if text(code, "enabled")? == "False" {
            continue;
        }
        if text(code, "enabled")? != "True" {
            return Err("invalid enabled flag".into());
        }
        let source = format!("{prefix}/code:{}", text(code, "id")?);
        if text(code, "type")? != "2" && code.children().any(|n| n.has_tag_name("expression")) {
            return Err(format!("{source}: expression container not translated"));
        }
        let children = child(code, "subitems")?;
        let kind = text(code, "type")?;
        let payload = child(code, "cmdcond")?;
        if (matches!(kind, "2") || (kind == "1" && text(code, "device")? != "0"))
            && payload.children().filter(|n| n.is_element()).count() != 1
        {
            return Err("ambiguous action payload".into());
        }
        if kind != "4" && kind != "5" {
            else_seen = false;
        }
        match kind {
            "5" => {
                if children.children().any(|n| n.is_element()) {
                    return Err("comment contains executable children".into());
                }
            }
            "4" => {
                if else_seen || payload.children().any(|n| n.is_element()) {
                    return Err("invalid or duplicate Else".into());
                }
                else_seen = true;
                let Some(Node {
                    action: Action::If { else_actions, .. },
                    ..
                }) = result.last_mut()
                else {
                    return Err("orphan Else".into());
                };
                if !else_actions.is_empty() {
                    return Err("duplicate Else".into());
                }
                *else_actions = sequence(children, bindings, &source, depth + 1, fades, motors)?;
            }
            "2" => result.push(Node {
                source: source.clone(),
                action: Action::If {
                    condition: compound_condition(code, bindings, depth, motors)?,
                    then_actions: sequence(children, bindings, &source, depth + 1, fades, motors)?,
                    else_actions: vec![],
                },
            }),
            "1" if text(code, "id")? == "0"
                && text(code, "device")? == "0"
                && !payload.children().any(|n| n.is_element()) =>
            {
                result.extend(sequence(children, bindings, &source, depth + 1, fades, motors)?)
            }
            "1" => {
                if children.children().any(|n| n.is_element()) {
                    return Err("action with nested executable children".into());
                }
                let payload=child(payload,"devicecommand")?;
                let action=if payload.attribute("owneridtype")!=Some("variable")
                    && text(payload,"command")?=="RAMP_TO_LEVEL" && fades.is_some() {
                    timed_fade(payload,text(code,"device")?,bindings,&source,fades.as_mut().unwrap())?
                } else {action_with_motor(payload,text(code,"device")?,bindings,motors)?};
                result.push(Node {source,action});
            }
            _ => return Err(format!("{source}: unsupported node type {kind}")),
        }
    }
    Ok(result)
}
pub fn compile_event(xml: &str, bindings: &BTreeMap<String, Binding>) -> Result<Vec<Node>, String> {
    compile_event_inner(xml,bindings,None,None).map(|event|event.actions)
}
/// Opt-in conversion returns both AST references and disabled timed definitions.
/// Callers must retain both; the legacy API still rejects timed fades.
pub fn compile_event_with_fades(xml:&str,bindings:&BTreeMap<String,Binding>,interval_ms:u64,lateness_ms:u64)->Result<CompiledEvent,String> {
    compile_event_inner(xml,bindings,Some(FadeContext{definitions:Default::default(),interval_ms,lateness_ms}),None)
}
/// Scoped opt-in. All source conditions remain guarded and actual-only. The
/// result is an unexecutable draft; mapping review never enables a source rule.
pub fn compile_event_with_motors(xml:&str,bindings:&BTreeMap<String,Binding>,options:MotorOptions<'_>)->Result<CompiledEvent,String> {
    compile_event_inner(xml,bindings,None,Some(options))
}
pub fn compile_event_with_motors_and_fades(xml:&str,bindings:&BTreeMap<String,Binding>,options:MotorOptions<'_>,interval_ms:u64,lateness_ms:u64)->Result<CompiledEvent,String> {
    compile_event_inner(xml,bindings,Some(FadeContext{definitions:Default::default(),interval_ms,lateness_ms}),Some(options))
}
fn compile_event_inner(xml:&str,bindings:&BTreeMap<String,Binding>,mut fades:Option<FadeContext>,motors:Option<MotorOptions<'_>>)->Result<CompiledEvent,String> {
    if xml.len() > 4 * 1024 * 1024 {
        return Err("event XML too large".into());
    }
    let doc = roxmltree::Document::parse(xml).map_err(|e| e.to_string())?;
    let event = doc.root_element();
    if !event.has_tag_name("event")
        || event
            .descendants()
            .filter(|n| n.has_tag_name("codeitem"))
            .count()
            > 4096
    {
        return Err("invalid event document".into());
    }
    let mut ids = std::collections::BTreeSet::new();
    for code in event.descendants().filter(|n| n.has_tag_name("codeitem")) {
        if !ids.insert(text(code, "id")?) {
            return Err("duplicate source node id".into());
        }
    }
    let prefix = format!(
        "device:{}/event:{}",
        text(event, "deviceid")?,
        text(event, "eventid")?
    );
    let motors=motors.map(|options|MotorContext{options,source_key:format!("{}:{}",text(event,"deviceid").unwrap(),text(event,"eventid").unwrap()),sha256:xml_sha256(xml)});
    if motors.as_ref().is_some_and(|m|m.options.reviews.len()>256){return Err("motor review scope exceeds 256".into());}
    let actions=sequence(event, bindings, &prefix, 0, &mut fades,motors.as_ref())?;
    Ok(CompiledEvent{actions,timed_scenes:fades.map(|v|v.definitions).unwrap_or_default()})
}

#[derive(Debug, serde::Serialize)]
pub struct Disposition {
    pub owner: String,
    pub syntax_compiled: bool,
    pub enabled: bool,
    pub compiled_nodes: usize,
    pub blocker: Option<String>,
    pub binding_basis: &'static str,
}
/// Candidate bindings derived from the project hierarchy are for syntax audit
/// only. No physical model, mode or ownership verification is inferred.
pub(crate) fn dispositions(
    document: &roxmltree::Document<'_>,
    devices: &[crate::Device],
) -> Vec<Disposition> {
    let bindings = candidate_bindings(document, devices);
    fn count(nodes: &[Node]) -> usize {
        nodes
            .iter()
            .map(|n| {
                1 + match &n.action {
                    Action::If {
                        then_actions,
                        else_actions,
                        ..
                    } => count(then_actions) + count(else_actions),
                    _ => 0,
                }
            })
            .sum()
    }
    document.root_element().children().find(|n|n.has_tag_name("event_mgr")).into_iter()
        .flat_map(|m|m.children().filter(|n|n.has_tag_name("event")))
        .map(|event| {
            let owner=format!("event:{}:{}",crate::text(event,"deviceid").unwrap_or("?"),crate::text(event,"eventid").unwrap_or("?"));
            let result=compile_event(&document.input_text()[event.range()],&bindings);
            let (syntax_compiled,compiled_nodes,blocker)=match result {Ok(nodes)=>(true,count(&nodes),None),Err(e)=>(false,0,Some(e))};
            Disposition {owner,syntax_compiled,compiled_nodes,blocker,enabled:false,binding_basis:"source hierarchy only; physical bindings and scene semantics need review"}
        }).collect()
}

/// Source-hierarchy candidates for read-only audits, never verified bindings.
pub(crate) fn candidate_bindings(
    document: &roxmltree::Document<'_>,
    devices: &[crate::Device],
) -> BTreeMap<String, Binding> {
    let mut bindings = BTreeMap::new();
    for device in devices {
        let kind = match device.kind.as_str() {
            "switch" => ModuleKind::Switch,
            "dimmer" => ModuleKind::Dimmer,
            "motor" => ModuleKind::Motor,
            "keypad" => ModuleKind::Keypad,
            _ => continue,
        };
        if let Ok(id) = DeviceId::new(device.stable_id.clone()) {
            bindings.insert(device.control4_id.clone(), Binding { device: id, kind });
        }
    }
    for item in document.descendants().filter(|n| n.has_tag_name("item")) {
        let Some(id) = crate::text(item, "id") else {
            continue;
        };
        if bindings.contains_key(id) {
            continue;
        }
        let Some(parent) = item.ancestors().skip(1).find(|n| n.has_tag_name("item")) else {
            continue;
        };
        let Some(parent_id) = crate::text(parent, "id") else {
            continue;
        };
        if let Some(binding) = bindings.get(parent_id) {
            let kind = binding.kind;
            let device = binding.device.clone();
            bindings.insert(id.into(), Binding { device, kind });
        }
    }
    bindings
}
