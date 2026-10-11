//! Strict AST input for reviewed automation. Parsing is side-effect free and
//! never enables a rule merely because its syntax can be converted.
use crate::{Configuration, Error, invalid};
use homesense_core::{
    Command, DeviceId, LevelTransition, ModuleKind, MotorMotion, Percent,
    rule::{Action, Condition, Expr, Lifetime, Node, Value, ValueType, Variable},
};
use serde::Deserialize;
use std::collections::BTreeMap;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    schema_version: u32,
    configuration_revision: u64,
    variables: Vec<VariableInput>,
    rules: Vec<RuleInput>,
    #[serde(default)]
    immediate_scenes: Vec<SceneInput>,
    #[serde(default)]
    schedules: Vec<ScheduleInput>,
    #[serde(default)]
    gestures: Vec<GestureInput>,
    #[serde(default)]
    holds: Option<Holds>,
    #[serde(default)]
    timed_scenes: Vec<TimedSceneInput>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TimedSceneInput {
    id: String,
    #[serde(default)]
    enabled: bool,
    review_evidence: Option<String>,
    max_lateness_ms: u64,
    steps: Vec<TimedStepInput>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TimedStepInput {
    source: String,
    offset_ms: u64,
    device: String,
    command: CommandInput,
}
#[derive(Debug, Clone)]
pub struct TimedScene {
    pub enabled: bool,
    pub review_evidence: Option<String>,
    pub max_lateness_ms: u64,
    pub steps: Vec<homesense_core::scene::Step>,
}
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Holds {
    pub interval_ms: u64,
    pub increment: u8,
    pub bindings: Vec<Hold>,
}
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Hold {
    pub key: String,
    pub device: String,
    pub up: bool,
    #[serde(default)]
    pub enabled: bool,
    pub review_evidence: Option<String>,
    pub release: HoldRelease,
}
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HoldRelease {
    CancelPendingOnly,
    PauseRampFieldVerified,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SceneInput {
    id: String,
    review_evidence: String,
    activate: Vec<NodeInput>,
    deactivate: Option<Vec<NodeInput>>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ScheduleInput {
    id: String,
    timezone: String,
    weekdays: [bool; 7],
    hour: u8,
    minute: u8,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GestureInput {
    device: String,
    debounce_ms: u64,
    long_press_ms: u64,
    double_click_ms: u64,
    max_hold_ms: u64,
    double_click_enabled: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct VariableInput {
    id: String,
    kind: Kind,
    lifetime: Scope,
    feedback: Option<FeedbackBinding>,
}
#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum Kind {
    Bool,
    Integer,
}
#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum Scope {
    Persistent,
    Temporary,
    FeedbackCache,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RuleInput {
    id: String,
    #[serde(default)]
    enabled: bool,
    review_evidence: Option<String>,
    source: String,
    trigger: Trigger,
    actions: Vec<NodeInput>,
}
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Trigger {
    StartupSynchronized,
    VariableChanged { variable: String },
    DeviceChanged { device: String },
    SceneResult { scene: String },
    Schedule { schedule: String },
    Panel { device: String, event: PanelEvent },
}
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PanelEvent {
    Pressed,
    Released,
    Click,
    DoubleClick,
    LongStart,
    LongEnd,
    Cancelled,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NodeInput {
    source: String,
    action: ActionInput,
}
#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum ActionInput {
    Set {
        variable: String,
        value: ExprInput,
    },
    If {
        condition: ConditionInput,
        then_actions: Vec<NodeInput>,
        else_actions: Vec<NodeInput>,
    },
    Scene {
        id: String,
        activate: bool,
    },
    Command {
        device: String,
        command: CommandInput,
    },
}
#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum ExprInput {
    Bool {
        value: bool,
    },
    Integer {
        value: i64,
    },
    Variable {
        id: String,
    },
    DeviceOn {
        device: String,
    },
    DeviceLevel {
        device: String,
    },
    Add {
        left: Box<ExprInput>,
        right: Box<ExprInput>,
    },
    Subtract {
        left: Box<ExprInput>,
        right: Box<ExprInput>,
    },
}
#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum ConditionInput {
    Equal { left: ExprInput, right: ExprInput },
    Less { left: ExprInput, right: ExprInput },
    Not { condition: Box<ConditionInput> },
    All { conditions: Vec<ConditionInput> },
    Any { conditions: Vec<ConditionInput> },
    Unknown { value: ExprInput },
}
#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum CommandInput {
    Switch { on: bool },
    Level { value: u8, ramp: bool },
    PanelLed { on: bool },
    PauseRamp,
    MotorUp,
    MotorDown,
    MotorStop,
}
#[derive(Clone)]
pub struct Rule {
    pub id: String,
    pub enabled: bool,
    pub review_evidence: Option<String>,
    pub source: String,
    pub trigger: Trigger,
    pub actions: Vec<Node>,
}
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum FeedbackBinding {
    On { device: String },
    Level { device: String },
}
pub struct Document {
    pub revision: u64,
    pub variables: BTreeMap<String, Variable>,
    pub feedback_bindings: BTreeMap<String, FeedbackBinding>,
    pub rules: Vec<Rule>,
    pub immediate_scenes: BTreeMap<String, homesense_core::scene_expand::Definition>,
    pub schedules: Vec<homesense_core::calendar::Schedule>,
    pub gestures: BTreeMap<DeviceId, homesense_core::gesture::Settings>,
    pub holds: Option<Holds>,
    pub timed_scenes: BTreeMap<String, TimedScene>,
}
struct Compiler<'a> {
    config: &'a Configuration,
    variables: &'a BTreeMap<String, Variable>,
    remaining: usize,
}
fn identifier(s: &str) -> Result<(), Error> {
    if s.is_empty() || s.len() > 256 || s.chars().any(char::is_control) {
        Err(invalid("invalid automation identifier"))
    } else {
        Ok(())
    }
}
impl Compiler<'_> {
    fn charge(&mut self, depth: usize) -> Result<(), Error> {
        if self.remaining == 0 || depth > 32 {
            return Err(invalid("automation AST budget exceeded"));
        }
        self.remaining -= 1;
        Ok(())
    }
    fn device(&self, id: &str, kinds: &[ModuleKind]) -> Result<DeviceId, Error> {
        let id = DeviceId::new(id)?;
        let device = self
            .config
            .devices()
            .get(&id)
            .ok_or_else(|| invalid("unknown automation device"))?;
        let module = self.config.gateways()[&device.gateway]
            .modules()
            .get(&device.address)
            .ok_or_else(|| invalid("missing module"))?;
        if !kinds.contains(&module.kind()) {
            return Err(invalid("automation device capability mismatch"));
        }
        Ok(id)
    }
    fn variable(&self, id: &str) -> Result<&Variable, Error> {
        self.variables
            .get(id)
            .ok_or_else(|| invalid("unknown automation variable"))
    }
    fn expr(&mut self, e: ExprInput, d: usize) -> Result<(Expr, ValueType), Error> {
        self.charge(d)?;
        Ok(match e {
            ExprInput::Bool { value } => (Expr::Literal(Value::Bool(value)), ValueType::Bool),
            ExprInput::Integer { value } => {
                (Expr::Literal(Value::Integer(value)), ValueType::Integer)
            }
            ExprInput::Variable { id } => {
                let kind = self.variable(&id)?.value_type;
                (Expr::Variable(id), kind)
            }
            ExprInput::DeviceOn { device } => (
                Expr::DeviceOn(self.device(&device, &[ModuleKind::Switch, ModuleKind::Dimmer])?),
                ValueType::Bool,
            ),
            ExprInput::DeviceLevel { device } => (
                Expr::DeviceLevel(self.device(&device, &[ModuleKind::Dimmer])?),
                ValueType::Integer,
            ),
            ExprInput::Add { left, right } => {
                let (a, b) = self.integers(*left, *right, d + 1)?;
                (Expr::Add(Box::new(a), Box::new(b)), ValueType::Integer)
            }
            ExprInput::Subtract { left, right } => {
                let (a, b) = self.integers(*left, *right, d + 1)?;
                (Expr::Subtract(Box::new(a), Box::new(b)), ValueType::Integer)
            }
        })
    }
    fn integers(&mut self, a: ExprInput, b: ExprInput, d: usize) -> Result<(Expr, Expr), Error> {
        let (a, ak) = self.expr(a, d)?;
        let (b, bk) = self.expr(b, d)?;
        if ak != ValueType::Integer || bk != ak {
            return Err(invalid("integer operands required"));
        }
        Ok((a, b))
    }
    fn condition(&mut self, c: ConditionInput, d: usize) -> Result<Condition, Error> {
        self.charge(d)?;
        Ok(match c {
            ConditionInput::Equal { left, right } => {
                let (a, ak) = self.expr(left, d + 1)?;
                let (b, bk) = self.expr(right, d + 1)?;
                if ak != bk {
                    return Err(invalid("condition type mismatch"));
                }
                Condition::Equal(a, b)
            }
            ConditionInput::Less { left, right } => {
                let (a, b) = self.integers(left, right, d + 1)?;
                Condition::Less(a, b)
            }
            ConditionInput::Not { condition } => {
                Condition::Not(Box::new(self.condition(*condition, d + 1)?))
            }
            ConditionInput::All { conditions } => Condition::All(
                conditions
                    .into_iter()
                    .map(|c| self.condition(c, d + 1))
                    .collect::<Result<_, _>>()?,
            ),
            ConditionInput::Any { conditions } => Condition::Any(
                conditions
                    .into_iter()
                    .map(|c| self.condition(c, d + 1))
                    .collect::<Result<_, _>>()?,
            ),
            ConditionInput::Unknown { value } => Condition::Unknown(self.expr(value, d + 1)?.0),
        })
    }
    fn nodes(&mut self, nodes: Vec<NodeInput>, d: usize) -> Result<Vec<Node>, Error> {
        nodes
            .into_iter()
            .map(|node| {
                self.charge(d)?;
                if node.source.is_empty() || node.source.len() > 8192 {
                    return Err(invalid("missing or excessive source"));
                }
                let action = match node.action {
                    ActionInput::Set { variable, value } => {
                        let v = self.variable(&variable)?;
                        let kind = v.value_type;
                        if v.lifetime == Lifetime::FeedbackCache {
                            return Err(invalid("feedback cache is read only"));
                        }
                        let (value, actual) = self.expr(value, d + 1)?;
                        if actual != kind {
                            return Err(invalid("assignment type mismatch"));
                        }
                        Action::Set { variable, value }
                    }
                    ActionInput::If {
                        condition,
                        then_actions,
                        else_actions,
                    } => Action::If {
                        condition: self.condition(condition, d + 1)?,
                        then_actions: self.nodes(then_actions, d + 1)?,
                        else_actions: self.nodes(else_actions, d + 1)?,
                    },
                    ActionInput::Scene { id, activate } => {
                        identifier(&id)?;
                        Action::Scene { id, activate }
                    }
                    ActionInput::Command { device, command } => {
                        let (kind, command) = match command {
                            CommandInput::Switch { on } => {
                                (ModuleKind::Switch, Command::Switch(on))
                            }
                            CommandInput::Level { value, ramp } => (
                                ModuleKind::Dimmer,
                                Command::SetLevel {
                                    level: Percent::new(value)?,
                                    transition: if ramp {
                                        LevelTransition::Ramp
                                    } else {
                                        LevelTransition::Immediate
                                    },
                                },
                            ),
                            CommandInput::PanelLed { on } => {
                                (ModuleKind::Keypad, Command::PanelLed(on))
                            }
                            CommandInput::PauseRamp => (ModuleKind::Dimmer, Command::PauseRamp),
                            CommandInput::MotorUp => {
                                (ModuleKind::Motor, Command::Motor(MotorMotion::Up))
                            }
                            CommandInput::MotorDown => {
                                (ModuleKind::Motor, Command::Motor(MotorMotion::Down))
                            }
                            CommandInput::MotorStop => {
                                (ModuleKind::Motor, Command::Motor(MotorMotion::Stop))
                            }
                        };
                        Action::Command {
                            device: self.device(&device, &[kind])?,
                            command,
                        }
                    }
                };
                Ok(Node {
                    source: node.source,
                    action,
                })
            })
            .collect()
    }
}
impl Document {
    pub fn parse(bytes: &[u8], config: &Configuration) -> Result<Self, Error> {
        if bytes.len() > 4 * 1024 * 1024 {
            return Err(invalid("automation document too large"));
        }
        let input: Input = serde_json::from_slice(bytes).map_err(Error::Json)?;
        if input.schema_version != 1
            || input.configuration_revision != config.revision()
            || input.variables.len() > 1024
            || input.rules.len() > 256
        {
            return Err(invalid("automation version or capacity mismatch"));
        }
        let mut variables = BTreeMap::new();
        let mut feedback_bindings = BTreeMap::new();
        for v in input.variables {
            identifier(&v.id)?;
            if matches!(v.lifetime, Scope::FeedbackCache) != v.feedback.is_some() {
                return Err(invalid(
                    "feedback cache requires a binding and other scopes forbid it",
                ));
            }
            if let Some(binding) = v.feedback {
                let (id, kinds, correct) = match &binding {
                    FeedbackBinding::On { device } => (
                        device,
                        vec![ModuleKind::Switch, ModuleKind::Dimmer],
                        matches!(v.kind, Kind::Bool),
                    ),
                    FeedbackBinding::Level { device } => (
                        device,
                        vec![ModuleKind::Dimmer],
                        matches!(v.kind, Kind::Integer),
                    ),
                };
                if !correct {
                    return Err(invalid("feedback binding type mismatch"));
                }
                Compiler {
                    config,
                    variables: &variables,
                    remaining: 0,
                }
                .device(id, &kinds)?;
                feedback_bindings.insert(v.id.clone(), binding);
            }
            if variables
                .insert(
                    v.id,
                    Variable {
                        value_type: match v.kind {
                            Kind::Bool => ValueType::Bool,
                            Kind::Integer => ValueType::Integer,
                        },
                        lifetime: match v.lifetime {
                            Scope::Persistent => Lifetime::Persistent,
                            Scope::Temporary => Lifetime::Temporary,
                            Scope::FeedbackCache => Lifetime::FeedbackCache,
                        },
                        value: None,
                    },
                )
                .is_some()
            {
                return Err(invalid("duplicate variable"));
            }
        }
        let mut compiler = Compiler {
            config,
            variables: &variables,
            remaining: 65536,
        };
        if input.immediate_scenes.len() > 1024
            || input.schedules.len() > 256
            || input.gestures.len() > 256
        {
            return Err(invalid("automation definition capacity exceeded"));
        }
        let mut immediate_scenes = BTreeMap::new();
        for scene in input.immediate_scenes {
            identifier(&scene.id)?;
            if scene.review_evidence.trim().is_empty() || scene.review_evidence.len() > 8192 {
                return Err(invalid("scene review evidence required"));
            }
            let definition = homesense_core::scene_expand::Definition {
                activate: compiler.nodes(scene.activate, 0)?,
                deactivate: scene
                    .deactivate
                    .map(|nodes| compiler.nodes(nodes, 0))
                    .transpose()?,
            };
            if immediate_scenes.insert(scene.id, definition).is_some() {
                return Err(invalid("duplicate scene"));
            }
        }
        for (id, scene) in &immediate_scenes {
            for activate in [true, false] {
                if !activate && scene.deactivate.is_none() {
                    continue;
                }
                homesense_core::scene_expand::expand(
                    &[Node {
                        source: format!("scene:{id}"),
                        action: Action::Scene {
                            id: id.clone(),
                            activate,
                        },
                    }],
                    &immediate_scenes,
                    1024,
                )
                .map_err(|e| invalid(format!("scene {id}: {e:?}")))?;
            }
        }
        let mut schedules = vec![];
        for schedule in input.schedules {
            identifier(&schedule.id)?;
            if schedule.timezone != "Asia/Shanghai" {
                return Err(invalid("unsupported calendar timezone"));
            }
            schedules.push(homesense_core::calendar::Schedule {
                id: schedule.id,
                weekdays: schedule.weekdays,
                hour: schedule.hour,
                minute: schedule.minute,
            });
        }
        homesense_core::calendar::Calendar::new(schedules.clone(), 946684800000, 0, None, 1000)
            .map_err(|e| invalid(format!("calendar: {e:?}")))?;
        let mut gestures = BTreeMap::new();
        for key in input.gestures {
            let id = compiler.device(&key.device, &[ModuleKind::Keypad])?;
            let settings = homesense_core::gesture::Settings {
                debounce_ms: key.debounce_ms,
                long_press_ms: key.long_press_ms,
                double_click_ms: key.double_click_ms,
                max_hold_ms: key.max_hold_ms,
                double_click_enabled: key.double_click_enabled,
            };
            homesense_core::gesture::KeyGesture::new(settings, 0)
                .map_err(|e| invalid(format!("gesture: {e:?}")))?;
            if gestures.insert(id, settings).is_some() {
                return Err(invalid("duplicate gesture key"));
            }
        }
        if let Some(holds) = &input.holds {
            if !(100..=1000).contains(&holds.interval_ms)
                || !(1..=20).contains(&holds.increment)
                || holds.bindings.len() > 256
            {
                return Err(invalid("invalid hold bus budget"));
            }
            let mut keys = std::collections::BTreeSet::new();
            for hold in &holds.bindings {
                let key = compiler.device(&hold.key, &[ModuleKind::Keypad])?;
                let device = compiler.device(&hold.device, &[ModuleKind::Dimmer])?;
                if !keys.insert(key.clone()) {
                    return Err(invalid("duplicate hold key"));
                }
                if hold.enabled
                    && (hold
                        .review_evidence
                        .as_ref()
                        .is_none_or(|s| s.trim().is_empty() || s.len() > 8192)
                        || !gestures.contains_key(&key)
                        || config.binding(&key).is_none()
                        || config.binding(&device).is_none())
                {
                    return Err(invalid(
                        "enabled hold requires review, gestures and enabled devices",
                    ));
                }
            }
        }
        if input.timed_scenes.len() > 1024 {
            return Err(invalid("timed scene capacity exceeded"));
        }
        let mut timed_scenes = BTreeMap::new();
        for scene in input.timed_scenes {
            identifier(&scene.id)?;
            if immediate_scenes.contains_key(&scene.id) || timed_scenes.contains_key(&scene.id) {
                return Err(invalid("duplicate scene identity"));
            }
            if scene.enabled
                && scene
                    .review_evidence
                    .as_ref()
                    .is_none_or(|s| s.trim().is_empty() || s.len() > 8192)
            {
                return Err(invalid("enabled timed scene requires review evidence"));
            }
            let mut steps = vec![];
            for step in scene.steps {
                let node = compiler
                    .nodes(
                        vec![NodeInput {
                            source: step.source,
                            action: ActionInput::Command {
                                device: step.device,
                                command: step.command,
                            },
                        }],
                        0,
                    )?
                    .remove(0);
                let Action::Command { device, command } = node.action else {
                    unreachable!()
                };
                if scene.enabled && config.binding(&device).is_none() {
                    return Err(invalid("enabled timed scene references disabled output"));
                }
                steps.push(homesense_core::scene::Step {
                    device,
                    command,
                    delay_ms: step.offset_ms,
                    source: node.source,
                });
            }
            homesense_core::scene::Timeline::new(
                steps.clone(),
                homesense_core::queue::Epochs {
                    configuration: input.configuration_revision,
                    transport: 1,
                    control: 1,
                },
                0,
                scene.max_lateness_ms,
            )
            .map_err(|e| invalid(format!("timed scene: {e:?}")))?;
            timed_scenes.insert(
                scene.id,
                TimedScene {
                    enabled: scene.enabled,
                    review_evidence: scene.review_evidence,
                    max_lateness_ms: scene.max_lateness_ms,
                    steps,
                },
            );
        }
        let mut ids = std::collections::BTreeSet::new();
        let mut rules = vec![];
        for rule in input.rules {
            identifier(&rule.id)?;
            if !ids.insert(rule.id.clone())
                || rule.source.is_empty()
                || rule.source.len() > 8192
                || (rule.enabled
                    && rule
                        .review_evidence
                        .as_ref()
                        .is_none_or(|s| s.trim().is_empty()))
            {
                return Err(invalid("invalid rule identity or missing review evidence"));
            }
            match &rule.trigger {
                Trigger::VariableChanged { variable } => {
                    compiler.variable(variable)?;
                }
                Trigger::DeviceChanged { device } => {
                    compiler.device(device, &[ModuleKind::Switch, ModuleKind::Dimmer])?;
                }
                Trigger::Panel { device, .. } => {
                    compiler.device(device, &[ModuleKind::Keypad])?;
                }
                Trigger::SceneResult { scene } => identifier(scene)?,
                Trigger::Schedule { schedule } => identifier(schedule)?,
                Trigger::StartupSynchronized => {}
            }
            rules.push(Rule {
                id: rule.id,
                enabled: rule.enabled,
                review_evidence: rule.review_evidence,
                source: rule.source,
                trigger: rule.trigger,
                actions: compiler.nodes(rule.actions, 0)?,
            });
        }
        Ok(Self {
            revision: input.configuration_revision,
            variables,
            feedback_bindings,
            rules,
            immediate_scenes,
            schedules,
            gestures,
            holds: input.holds,
            timed_scenes,
        })
    }
}

impl Document {
    /// Link reviewed immediate scenes and schedule identifiers without enabling
    /// disabled rules. All enabled references must resolve before construction.
    pub fn linked_rules(
        &self,
        scenes: &BTreeMap<String, homesense_core::scene_expand::Definition>,
        schedules: &std::collections::BTreeSet<String>,
    ) -> Result<Vec<Rule>, Error> {
        self.link_rules(scenes,schedules,false)
    }
    /// Explicit opt-in for adapters that atomically accept and install delayed
    /// calls. Ordinary immediate adapters must continue using linked_rules.
    pub fn linked_rules_with_deferred(&self)->Result<Vec<Rule>,Error> {
        self.link_rules(&self.immediate_scenes,&self.schedules.iter().map(|s|s.id.clone()).collect(),true)
    }
    fn link_rules(&self,scenes:&BTreeMap<String,homesense_core::scene_expand::Definition>,schedules:&std::collections::BTreeSet<String>,allow_deferred:bool)->Result<Vec<Rule>,Error> {
        let deferred=if allow_deferred {self.timed_scenes.iter().filter(|(_,s)|s.enabled).map(|(id,_)|id.clone()).collect()} else {Default::default()};
        let mut linked = vec![];
        for rule in self.rules.iter().filter(|r| r.enabled) {
            match &rule.trigger {
                Trigger::SceneResult { scene }
                    if !scenes.contains_key(scene)
                        && !self.timed_scenes.get(scene).is_some_and(|s| s.enabled) =>
                {
                    return Err(invalid(format!(
                        "{}: unknown scene trigger {scene}",
                        rule.id
                    )));
                }
                Trigger::Schedule { schedule } if !schedules.contains(schedule) => {
                    return Err(invalid(format!("{}: unknown schedule {schedule}", rule.id)));
                }
                _ => {}
            }
            let mut actions = homesense_core::scene_expand::expand_with_deferred(&rule.actions, scenes, &deferred, 1024)
                .map_err(|e| invalid(format!("{}: {e:?}", rule.id)))?;
            for variable in homesense_core::rule::variable_references_with_deferred(&actions,&deferred).map_err(invalid)? {
                if !self.variables.contains_key(&variable) {
                    return Err(invalid(format!(
                        "{}: undeclared scene variable {variable}",
                        rule.id
                    )));
                }
            }
            lower_feedback(&mut actions, &self.feedback_bindings)?;
            let mut rule = rule.clone();
            rule.actions = actions;
            linked.push(rule);
        }
        Ok(linked)
    }
}

// Lower read-only aliases to actual device operands before planning. This makes
// the existing feedback freshness/read-set lease cover the source device too.
fn lower_feedback(
    nodes: &mut [Node],
    bindings: &BTreeMap<String, FeedbackBinding>,
) -> Result<(), Error> {
    fn expr(e: &mut Expr, bindings: &BTreeMap<String, FeedbackBinding>) -> Result<(), Error> {
        match e {
            Expr::Variable(name) => {
                if let Some(binding) = bindings.get(name) {
                    *e = match binding {
                        FeedbackBinding::On { device } => Expr::DeviceOn(DeviceId::new(device)?),
                        FeedbackBinding::Level { device } => {
                            Expr::DeviceLevel(DeviceId::new(device)?)
                        }
                    };
                }
            }
            Expr::Add(a, b) | Expr::Subtract(a, b) => {
                expr(a, bindings)?;
                expr(b, bindings)?;
            }
            _ => {}
        }
        Ok(())
    }
    fn condition(
        c: &mut Condition,
        bindings: &BTreeMap<String, FeedbackBinding>,
    ) -> Result<(), Error> {
        match c {
            Condition::Equal(a, b) | Condition::Less(a, b) => {
                expr(a, bindings)?;
                expr(b, bindings)?;
            }
            Condition::Not(c) => condition(c, bindings)?,
            Condition::All(cs) | Condition::Any(cs) => {
                for c in cs {
                    condition(c, bindings)?;
                }
            }
            Condition::Unknown(e) => expr(e, bindings)?,
        }
        Ok(())
    }
    for node in nodes {
        match &mut node.action {
            Action::Set { variable, value } => {
                if bindings.contains_key(variable) {
                    return Err(invalid("scene cannot write feedback cache"));
                }
                expr(value, bindings)?;
            }
            Action::If {
                condition: c,
                then_actions,
                else_actions,
            } => {
                condition(c, bindings)?;
                lower_feedback(then_actions, bindings)?;
                lower_feedback(else_actions, bindings)?;
            }
            _ => {}
        }
    }
    Ok(())
}
