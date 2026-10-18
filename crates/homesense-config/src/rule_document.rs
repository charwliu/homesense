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
    #[serde(default)]
    component_mode_confirmations: Vec<crate::component_modes::Policy>,
    #[serde(default)]
    away_off: Option<AwayOff>,
    #[serde(default)]
    scene_deactivations: Vec<SceneDeactivation>,
    schema_version: u32,
    configuration_revision: u64,
    variables: Vec<VariableInput>,
    #[serde(default)]
    variable_migrations: Vec<MigrationInput>,
    rules: Vec<RuleInput>,
    #[serde(default)]
    immediate_scenes: Vec<SceneInput>,
    #[serde(default)]
    scene_tracking: Vec<crate::scene_tracking::Policy>,
    #[serde(default)]
    scene_metadata: Vec<SceneMetadata>,
    #[serde(default)]
    schedules: Vec<ScheduleInput>,
    #[serde(default)]
    gestures: Vec<GestureInput>,
    #[serde(default)]
    holds: Option<Holds>,
    #[serde(default)]
    timed_scenes: Vec<TimedSceneInput>,
    #[serde(default)]
    scene_switch_groups: Vec<crate::scene_switches::Group>,
    #[serde(default)]
    indicator_links: Vec<crate::indicator_links::Link>,
    #[serde(default)]
    scene_observation: Option<crate::scene_observation::Settings>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MigrationInput {
    variable: String,
    source_revision: u64,
    source_type: Kind,
    conversion: MigrationConversion,
    review_evidence: String,
}
#[derive(Deserialize)]
#[serde(rename_all="snake_case")]
enum MigrationConversion { BoolToInteger, IntegerZeroOneToBool, ResetUnknown }
#[derive(Debug, Clone)]
pub struct VariableMigration {
    pub source_revision: u64,
    pub source_type: ValueType,
    pub conversion: homesense_core::variable_migration::Conversion,
    pub review_evidence: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TimedSceneInput {
    id: String,
    #[serde(default)]
    failure_policy: SceneFailurePolicy,
    #[serde(default)]
    enabled: bool,
    review_evidence: Option<String>,
    max_lateness_ms: u64,
    steps: Vec<TimedStepInput>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TimedStepInput {
    fade: Option<FadeTiming>,
    source: String,
    offset_ms: u64,
    device: String,
    command: CommandInput,
}
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FadeTiming {
    pub duration_ms: u64,
    pub interval_ms: u64,
    pub lateness_ms: u64,
}
impl FadeTiming {
    pub fn samples(&self) -> Result<u64, Error> {
        homesense_core::fade::Fade::new(Percent::new(0).unwrap(),Percent::new(100).unwrap(),0,
            self.duration_ms,self.interval_ms,self.lateness_ms,1024)
            .map(|fade|fade.reserved_samples()).map_err(|e|invalid(format!("fade timing: {e:?}")))
    }
}
/// Supported first-release policy: stop remaining actions after output failure.
/// No automatic retry or compensation of already written commands.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SceneFailurePolicy {
    #[default]
    CancelRemaining,
}
#[derive(Debug, Clone)]
pub struct TimedScene {
    pub failure_policy: SceneFailurePolicy,
    pub fades: BTreeMap<usize, FadeTiming>,
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
fn legacy_scene_enabled() -> bool { true }
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SceneInput {
    id: String,
    #[serde(default = "legacy_scene_enabled")]
    enabled: bool,
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
    Motor {device:String,action:LogicalMotorAction,mapping_review:String},
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
    MotorState {device:String,state:MotorStateInput,#[serde(default)]evidence_policy:MotorPolicyInput,#[serde(default="motor_max_age")]max_age_ms:u64,mapping_review:String,#[serde(default)]evidence_review:Option<String>},
    Equal { left: ExprInput, right: ExprInput },
    Less { left: ExprInput, right: ExprInput },
    Not { condition: Box<ConditionInput> },
    All { conditions: Vec<ConditionInput> },
    Any { conditions: Vec<ConditionInput> },
    Unknown { value: ExprInput },
}
#[derive(Deserialize)]
#[serde(rename_all="snake_case")]
enum LogicalMotorAction {Open,Close,Stop}
#[derive(Deserialize)]
#[serde(rename_all="snake_case")]
enum MotorStateInput {Moving,Stopped}
#[derive(Deserialize,Default)]
#[serde(rename_all="snake_case")]
enum MotorPolicyInput {#[default]ActualOnly,ReviewedCommandEstimate}
fn motor_max_age()->u64{1000}
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
#[derive(Clone,Debug,serde::Deserialize,serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct SceneMetadata {
    pub id:String,
    pub name:String,
    #[serde(default)]
    pub room:Option<String>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all="snake_case")]
pub enum SceneExecutionKind { Immediate, Timed }
/// Explicit new Away policy, separate from all imported scene definitions.
#[derive(Debug,Clone,Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AwayOff {
    #[serde(default)]
    pub hap_expose:bool,
    pub enabled:bool, pub review_evidence:String,
    pub active_execution_policy:AwayExecutionPolicy,
    pub modules:Vec<AwayModule>,
}
#[derive(Debug,Clone,Copy,Deserialize)]
#[serde(rename_all="snake_case")]
pub enum AwayExecutionPolicy {RequireQuiescentRuntime,CancelAffectedLighting}
#[derive(Debug,Clone,Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AwayModule {pub module_id:String,pub devices:Vec<String>,pub review_evidence:String}
#[derive(Clone)]
pub struct Document {
    pub component_mode_confirmations: Vec<crate::component_modes::Policy>,
    pub away_off: Option<AwayOff>,
    pub scene_deactivations: BTreeMap<String,SceneDeactivation>,
    pub revision: u64,
    pub scene_tracking: BTreeMap<String,crate::scene_tracking::Policy>,
    pub scene_metadata: BTreeMap<String,String>,
    /// Resolved from the same revision's room catalog; absent means unassigned.
    pub scene_rooms: BTreeMap<String,crate::Room>,
    pub variables: BTreeMap<String, Variable>,
    pub variable_migrations: BTreeMap<String, VariableMigration>,
    pub feedback_bindings: BTreeMap<String, FeedbackBinding>,
    pub rules: Vec<Rule>,
    pub immediate_scenes: BTreeMap<String, homesense_core::scene_expand::Definition>,
    /// Explicitly disabled imported definitions cannot be activated by enabling a caller alone.
    pub disabled_immediate_scenes: std::collections::BTreeSet<String>,
    pub schedules: Vec<homesense_core::calendar::Schedule>,
    pub gestures: BTreeMap<DeviceId, homesense_core::gesture::Settings>,
    pub holds: Option<Holds>,
    pub timed_scenes: BTreeMap<String, TimedScene>,
    pub scene_switch_groups: Vec<crate::scene_switches::Group>,
    pub indicator_links: Vec<crate::indicator_links::Link>,
    pub scene_observation: Option<crate::scene_observation::Settings>,
}
/// New links require an explicit runtime policy; no source timing default.
#[derive(Debug,Clone,Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SceneDeactivation {
    pub scene:String, pub target:String, pub enabled:bool, pub review_evidence:String,
    pub active_execution_policy:DeactivationPolicy,
}
#[derive(Debug,Clone,Copy,Deserialize,serde::Serialize)]
#[serde(rename_all="snake_case")]
pub enum DeactivationPolicy { RequireQuiescentRuntime }
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
            ConditionInput::MotorState {device,state,evidence_policy,max_age_ms,mapping_review,evidence_review}=>{
                let device=self.device(&device,&[ModuleKind::Motor])?;
                self.config.motor_command(&device,homesense_core::MotorAction::Stop)?;
                if mapping_review.trim().is_empty() || mapping_review.len()>8192 || !(1..=30000).contains(&max_age_ms)
                    || evidence_review.as_ref().is_some_and(|e|e.trim().is_empty()||e.len()>8192) {return Err(invalid("motor predicate needs explicit mapping scope and bounded freshness"));}
                Condition::MotorState(homesense_core::rule_motor::Predicate{device,
                    state:match state{MotorStateInput::Moving=>homesense_core::rule_motor::State::Moving,MotorStateInput::Stopped=>homesense_core::rule_motor::State::Stopped},
                    evidence:match evidence_policy{MotorPolicyInput::ActualOnly=>homesense_core::rule_motor::Evidence::ActualOnly,MotorPolicyInput::ReviewedCommandEstimate=>homesense_core::rule_motor::Evidence::ReviewedCommandEstimate},
                    max_age_ms,mapping_review,evidence_review})
            },
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
                    ActionInput::Motor {device,action,mapping_review}=>{
                        if mapping_review.trim().is_empty()||mapping_review.len()>8192{return Err(invalid("logical motor intent needs explicit mapping review"));}
                        let device=self.device(&device,&[ModuleKind::Motor])?;
                        let action=match action {LogicalMotorAction::Open=>homesense_core::MotorAction::Open,LogicalMotorAction::Close=>homesense_core::MotorAction::Close,LogicalMotorAction::Stop=>homesense_core::MotorAction::Stop};
                        let command=self.config.motor_command(&device,action)?;
                        Action::Command{device,command}
                    },
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
                        let device = self.device(&device, &[kind])?;
                        // Automation uses the same logical targets as manual
                        // control. A supply is owned by its enabled combined
                        // light; accepting it here would defer rejection until
                        // after earlier scene steps have already been written.
                        if self.config.devices().values().any(|d| {
                            d.enabled && d.supply_relay.as_ref() == Some(&device)
                        }) {
                            return Err(invalid("automation must target the logical light, not its supply relay"));
                        }
                        if self.config.devices()[&device].supply_relay.is_some()
                            && !matches!(command, Command::SetLevel { transition: LevelTransition::Immediate, .. })
                        {
                            return Err(invalid("combined light automation requires a normalized level target"));
                        }
                        Action::Command { device, command }
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
    /// A reviewed curtain owns its fixed logical LED outputs exclusively across
    /// both configuration documents. Disabled source rules remain review data.
    fn validate_curtain_led_owners(&self,config:&Configuration)->Result<(),Error> {
        let owned:std::collections::BTreeSet<_>=config.devices().values().filter_map(|d|d.curtain_state.as_ref())
            .flat_map(|s|s.leds.open.iter().chain(&s.leds.close).chain(&s.leds.stop)).collect();
        if owned.is_empty(){return Ok(());}
        let check=|id:&DeviceId,command:Command| {
            if matches!(command,Command::PanelLed(_)) && owned.contains(id) {
                Err(invalid("curtain LED has an independent enabled automation owner"))
            } else {Ok(())}
        };
        for link in self.indicator_links.iter().filter(|l|l.enabled) {
            for id in &link.leds {check(id,Command::PanelLed(false))?;}
        }
        for scene in self.timed_scenes.values().filter(|s|s.enabled) {
            for step in &scene.steps {check(&step.device,step.command)?;}
        }
        fn walk(actions:&[Node],check:&impl Fn(&DeviceId,Command)->Result<(),Error>)->Result<(),Error> {
            for node in actions {
                match &node.action {
                    Action::Command{device,command}=>check(device,*command)?,
                    Action::If{then_actions,else_actions,..}=>{walk(then_actions,check)?;walk(else_actions,check)?;},
                    _=>{},
                }
            }Ok(())
        }
        for scene in self.immediate_scenes.values() {
            walk(&scene.activate,&check)?;if let Some(off)=&scene.deactivate{walk(off,&check)?;}
        }
        for rule in self.rules.iter().filter(|r|r.enabled){walk(&rule.actions,&check)?;}
        Ok(())
    }
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
        if input.variable_migrations.len()>1024 {return Err(invalid("too many variable migrations"));}
        let mut variable_migrations=BTreeMap::new();
        for migration in input.variable_migrations {
            identifier(&migration.variable)?;
            let target=variables.get(&migration.variable).ok_or_else(||invalid("migration target missing"))?;
            if target.lifetime!=Lifetime::Persistent || migration.source_revision==0 || migration.source_revision>=input.configuration_revision
                || migration.review_evidence.trim().is_empty() || migration.review_evidence.len()>8192 {
                return Err(invalid("invalid variable migration scope or review"));
            }
            let source_type=match migration.source_type {Kind::Bool=>ValueType::Bool,Kind::Integer=>ValueType::Integer};
            use homesense_core::variable_migration::Conversion;
            let conversion=match migration.conversion {
                MigrationConversion::BoolToInteger=>Conversion::BoolToInteger,
                MigrationConversion::IntegerZeroOneToBool=>Conversion::IntegerZeroOneToBool,
                MigrationConversion::ResetUnknown=>Conversion::ResetUnknown,
            };
            conversion.apply(source_type,target.value_type,None).map_err(|_|invalid("invalid migration direction"))?;
            if variable_migrations.insert(migration.variable,VariableMigration{source_revision:migration.source_revision,source_type,conversion,review_evidence:migration.review_evidence}).is_some() {
                return Err(invalid("duplicate variable migration"));
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
        let mut disabled_immediate_scenes = std::collections::BTreeSet::new();
        for scene in input.immediate_scenes {
            identifier(&scene.id)?;
            if scene.review_evidence.trim().is_empty() || scene.review_evidence.len() > 8192 {
                return Err(invalid("scene review evidence required"));
            }
            if !scene.enabled { disabled_immediate_scenes.insert(scene.id.clone()); }
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
            let mut fades = BTreeMap::new();
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
                if let Some(fade)=step.fade {
                    fade.samples()?;
                    if !matches!(command, Command::SetLevel {transition:LevelTransition::Immediate,..}) {
                        return Err(invalid("software fade requires an immediate level target"));
                    }
                    if scene.enabled && config.binding(&device).is_some_and(|(_,module)| module.profile() == homesense_core::ProtocolProfile::Ad8Din) {
                        return Err(invalid("AD8 does not support arbitrary immediate levels required for software fade"));
                    }
                    fades.insert(steps.len(),fade);
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
                    failure_policy: scene.failure_policy,
                    fades,
                    enabled: scene.enabled,
                    review_evidence: scene.review_evidence,
                    max_lateness_ms: scene.max_lateness_ms,
                    steps,
                },
            );
        }
        crate::scene_switches::validate(&input.scene_switch_groups,&variables,&timed_scenes,config)?;
        crate::indicator_links::validate(&input.indicator_links,config,&input.scene_switch_groups,&timed_scenes)?;
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
        if let Some(settings)=&input.scene_observation {
            settings.validate(config,&input.scene_switch_groups,&rules,input.holds.as_ref())?;
        }
        let mut scene_deactivations=BTreeMap::new();
        if input.scene_deactivations.len()>1024{return Err(invalid("deactivation capacity exceeded"));}
        for link in input.scene_deactivations {
            identifier(&link.scene)?;identifier(&link.target)?;
            if link.scene==link.target || link.review_evidence.trim().is_empty() || link.review_evidence.len()>8192
                || !immediate_scenes.contains_key(&link.scene)&&!timed_scenes.contains_key(&link.scene)
                || !immediate_scenes.contains_key(&link.target)&&!timed_scenes.contains_key(&link.target)
                || immediate_scenes.get(&link.scene).is_some_and(|s|s.deactivate.is_some()) {return Err(invalid("invalid, ambiguous or unresolved deactivation link"));}
            let enabled=|id:&String|immediate_scenes.contains_key(id)&&!disabled_immediate_scenes.contains(id)||timed_scenes.get(id).is_some_and(|s|s.enabled);
            if link.enabled && (!enabled(&link.scene)||!enabled(&link.target)){return Err(invalid("enabled deactivation references disabled scene"));}
            if scene_deactivations.insert(link.scene.clone(),link).is_some(){return Err(invalid("duplicate deactivation link"));}
        }
        let scene_rooms=input.scene_metadata.iter().filter_map(|metadata| {
            metadata.room.as_ref().map(|id| {
                config.rooms().get(id).cloned().map(|room|(metadata.id.clone(),room))
                    .ok_or_else(||invalid("scene room does not exist"))
            })
        }).collect::<Result<BTreeMap<_,_>,_>>()?;
        let document=Self {
            away_off: input.away_off,
            scene_deactivations,
            revision: input.configuration_revision,
            variables,
            variable_migrations,
            feedback_bindings,
            component_mode_confirmations: input.component_mode_confirmations,
            rules,
            immediate_scenes,
            disabled_immediate_scenes,
            scene_tracking: input.scene_tracking.iter().map(|p|(p.scene.clone(),p.clone())).collect(),
            scene_metadata: input.scene_metadata.iter().map(|m|(m.id.clone(),m.name.clone())).collect(),
            scene_rooms,
            schedules,
            gestures,
            holds: input.holds,
            timed_scenes,
            scene_switch_groups:input.scene_switch_groups,
            indicator_links:input.indicator_links,
            scene_observation:input.scene_observation,
        };
        if let Some(away)=&document.away_off && away.enabled {
                if config.schema_version()!=2 || away.review_evidence.trim().is_empty() || away.review_evidence.len()>8192 || away.modules.is_empty() || away.modules.len()>128 {return Err(invalid("enabled Away operation requires reviewed stable modules"));}
                let mut seen=std::collections::BTreeSet::new();
                for reviewed in &away.modules {
                    if !seen.insert(&reviewed.module_id) || reviewed.review_evidence.trim().is_empty() || reviewed.review_evidence.len()>4096 {return Err(invalid("Away module review required"));}
                    let (_,module)=config.module(&reviewed.module_id).ok_or_else(||invalid("Away module missing"))?;
                    if !matches!(module.profile(),homesense_core::ProtocolProfile::R8i8DinPro|homesense_core::ProtocolProfile::Ad8Din) || matches!(module.verification(),homesense_core::Verification::Pending) || reviewed.devices.len()!=8 {return Err(invalid("Away module capability or coverage missing"));}
                    let mut channels=std::collections::BTreeSet::new();
                    for id in &reviewed.devices {
                        let id=DeviceId::new(id.clone())?;
                        let (binding,_) = config.binding(&id).ok_or_else(||invalid("Away lighting binding missing"))?;
                        if binding.module_id.as_deref()!=Some(reviewed.module_id.as_str()) || !channels.insert(binding.channel.get()) {return Err(invalid("Away coverage differs from module"));}
                    }
                    if channels.iter().copied().ne(1..=8) || config.logical_devices().iter().any(|(id,b)|b.module_id.as_deref()==Some(reviewed.module_id.as_str()) && !reviewed.devices.iter().any(|name|name==id.as_str())) {return Err(invalid("Away requires every physical output explicitly reviewed"));}
                }
        }
        crate::scene_tracking::validate(&input.scene_tracking,&document,config)?;
        if input.scene_metadata.len()!=document.scene_metadata.len() || input.scene_metadata.len()>1024 {return Err(invalid("duplicate/excessive scene metadata"));}
        for metadata in &input.scene_metadata {
            identifier(&metadata.id)?;
            if metadata.name.trim().is_empty() || metadata.name.len()>64 || metadata.name.chars().any(char::is_control)
                || (!document.immediate_scenes.contains_key(&metadata.id) && !document.timed_scenes.contains_key(&metadata.id)) {return Err(invalid("invalid scene display metadata"));}
        }
        for panel in config.keypads().values() {
            if panel.executes_events() {
                let controlled=panel.keys.values().map(|key|key.led_endpoint.id.as_str()).collect::<std::collections::BTreeSet<_>>();
                if document.rules.iter().any(|rule|rule.enabled && matches!(&rule.trigger,Trigger::Panel{device,..} if controlled.contains(device.as_str())))
                    || document.scene_observation.as_ref().is_some_and(|settings|settings.enabled && settings.bindings.iter().any(|binding|controlled.contains(binding.device.as_str())))
                    || document.holds.as_ref().is_some_and(|settings|settings.bindings.iter().any(|binding|binding.enabled&&controlled.contains(binding.key.as_str()))) {
                    return Err(invalid("configured keypad execution cannot share an endpoint with panel rules, scene observation or hold dimming"));
                }
            }
            for key in panel.keys.values() {
                if let Some(crate::keypad::LocalIndicator::SceneSelection{group,value,..})=&key.local_indicator {
                    let Some(crate::keypad::KeyAction::SceneToggle{scene,..})=&key.action else{return Err(invalid("local indicator scene action missing"));};
                    if !document.scene_switch_groups.iter().any(|candidate|&candidate.id==group && candidate.members.iter().any(|member|&member.scene==scene && member.value==*value)) {
                        return Err(invalid("local indicator must match the exact reviewed selection member"));
                    }
                }
                if let Some(crate::keypad::KeyAction::AwayOff{indicator_reset,..})=&key.action
                    && panel.enabled && key.led_endpoint.enabled {
                    if document.away_off.as_ref().is_none_or(|policy|!policy.enabled) {return Err(invalid("keypad Away requires enabled reviewed Away policy"));}
                    if indicator_reset.is_some() && config.module(&panel.module_id).is_none_or(|(_,m)|m.led_mode()!=homesense_core::PanelLedMode::Manual) {return Err(invalid("Away indicator reset requires reviewed manual panel mode"));}
                }
                if let Some(crate::keypad::KeyAction::SceneExecute { scene, .. }) = &key.action {
                    if document.scene_execution_kind(scene)!=Some(SceneExecutionKind::Immediate) || !document.scene_action_available(scene) {return Err(invalid("keypad execute requires enabled reviewed manual immediate scene"));}
                    crate::keypad::scene_execute_devices(&document,config,scene)?;
                }
                if let Some(crate::keypad::KeyAction::SceneToggle { scene, .. }) = &key.action
                    && (!document.scene_action_available(scene)
                        || !document.scene_switch_groups.iter().any(|group| group.members.iter().any(|member| &member.scene == scene))) {
                    return Err(invalid("keypad action requires an enabled reviewed selection scene"));
                }
            }
        }
        crate::component_modes::validate(&document.component_mode_confirmations,&document,config)?;
        crate::keypad_completion::validate_document(config, &document)?;
        document.validate_curtain_led_owners(config)?;
        document.validate_enabled_motor_predicates(config)?;
        if let Some(settings)=&document.scene_observation {settings.validate_linked(&document)?;}
        Ok(document)
    }
}

impl Document {
    fn validate_enabled_motor_predicates(&self,config:&Configuration)->Result<(),Error> {
        let mut any=false;
        for rule in &self.rules {any|=!homesense_core::rule_motor::requests(&rule.actions).map_err(invalid)?.is_empty();}
        for scene in self.immediate_scenes.values(){any|=!homesense_core::rule_motor::requests(&scene.activate).map_err(invalid)?.is_empty();if let Some(off)=&scene.deactivate{any|=!homesense_core::rule_motor::requests(off).map_err(invalid)?.is_empty();}}
        if !any{return Ok(());}

        fn walk(nodes:&[Node],doc:&Document,config:&Configuration,left:&mut usize,depth:usize)->Result<(),Error>{
            if depth>64{return Err(invalid("motor predicate scope nesting exceeded"));}
            for node in nodes {
                if *left==0{return Err(invalid("motor predicate scope budget exceeded"));}*left-=1;
                match &node.action {
                    Action::If{condition,then_actions,else_actions}=>{
                        let probe=Node{source:node.source.clone(),action:Action::If{condition:condition.clone(),then_actions:vec![],else_actions:vec![]}};
                        for request in homesense_core::rule_motor::requests(&[probe]).map_err(invalid)? {
                            let p=request.predicate;
                            if p.evidence!=homesense_core::rule_motor::Evidence::ReviewedCommandEstimate
                                || p.evidence_review.as_ref().is_none_or(|e|e.trim().is_empty())
                                || config.devices().get(&p.device).is_none_or(|d|d.curtain_state.is_none()) {
                                return Err(invalid("enabled motor condition needs per-predicate estimate review and an explicit shared model; no actual provider is available"));
                            }
                        }
                        walk(then_actions,doc,config,left,depth+1)?;walk(else_actions,doc,config,left,depth+1)?;
                    },
                    Action::Scene{id,activate}=>if let Some(scene)=doc.immediate_scenes.get(id)
                        && let Some(nodes)=if *activate{Some(&scene.activate)}else{scene.deactivate.as_ref()}{walk(nodes,doc,config,left,depth+1)?;},
                    Action::Motor{..}=>return Err(invalid("unresolved logical motor action")),
                    _=>{},
                }
            }Ok(())
        }
        for rule in self.rules.iter().filter(|r|r.enabled) {walk(&rule.actions,self,config,&mut 65536,0)?;}
        // Enabled scenes are now directly executable, not only reachable from
        // an enabled rule. The same per-predicate model/review gate applies.
        for (id,scene) in self.immediate_scenes.iter().filter(|(id,_)|!self.disabled_immediate_scenes.contains(*id)) {
            walk(&scene.activate,self,config,&mut 65536,0).map_err(|e|invalid(format!("manual scene {id}: {e}")))?;
            if let Some(off)=&scene.deactivate{walk(off,self,config,&mut 65536,0)?;}
        }
        Ok(())
    }
    /// Exact active source/node scopes used by the admission-owned read lease.
    pub fn enabled_motor_requests(&self)->Result<Vec<homesense_core::rule_motor::Request>,Error> {
        let mut requests=vec![];
        for rule in self.linked_rules_with_deferred()? {
            for request in homesense_core::rule_motor::requests(&rule.actions).map_err(invalid)? {
                if !requests.contains(&request){requests.push(request);}
                if requests.len()>256{return Err(invalid("active motor predicate scope exceeds 256"));}
            }
        }
        for id in self.immediate_scenes.keys().filter(|id|!self.disabled_immediate_scenes.contains(*id)) {
            // An unresolved disabled dependency is not admitted by the manual
            // linker and contributes no allowed operand scope here.
            if let Ok(actions)=self.linked_manual_scenes(std::slice::from_ref(id)) {
                for request in homesense_core::rule_motor::requests(&actions).map_err(invalid)? {
                    if !requests.contains(&request){requests.push(request);}
                    if requests.len()>256{return Err(invalid("active motor predicate scope exceeds 256"));}
                }
            }
        }
        Ok(requests)
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
    /// Kind and enabled gate are shared by publication, catalogs and execution.
    /// Immediate review evidence is mandatory at parse time; disabled imports remain held.
    pub fn scene_execution_kind(&self,id:&str)->Option<SceneExecutionKind> {
        if self.immediate_scenes.contains_key(id) && !self.disabled_immediate_scenes.contains(id) {
            Some(SceneExecutionKind::Immediate)
        } else if self.timed_scenes.get(id).is_some_and(|s|s.enabled && s.review_evidence.as_ref().is_some_and(|v|!v.trim().is_empty())) {
            Some(SceneExecutionKind::Timed)
        } else {None}
    }
    pub fn scene_action_available(&self,id:&str)->bool {
        match self.scene_execution_kind(id) {
            Some(SceneExecutionKind::Timed)=>true,
            Some(SceneExecutionKind::Immediate)=>self.linked_manual_scenes(&[id.to_owned()]).is_ok(),
            None=>false,
        }
    }
    pub fn scene_catalog(&self)->BTreeMap<String,String> {
        self.immediate_scenes.keys().chain(self.timed_scenes.keys())
            .filter(|id|self.scene_action_available(id))
            .map(|id|(id.clone(),self.scene_metadata.get(id).cloned().unwrap_or_else(||id.clone()))).collect()
    }
    /// A real manual entry AST, not a generated enabled rule or zero-delay timeline.
    /// Enabled definitions and nested disabled gates are checked before evaluation.
    pub fn linked_manual_scenes(&self,ids:&[String])->Result<Vec<Node>,Error> {
        if ids.is_empty() || ids.len()>64 || ids.iter().collect::<std::collections::BTreeSet<_>>().len()!=ids.len()
            || ids.iter().any(|id|self.scene_execution_kind(id).is_none()) {
            return Err(invalid("manual scene missing, disabled, duplicated or unreviewed"));
        }
        let roots=ids.iter().map(|id|Node{source:format!("manual-scene/{id}"),action:Action::Scene{id:id.clone(),activate:true}}).collect::<Vec<_>>();
        self.linked_manual_actions(&roots)
    }
    /// Trusted runtime composition can retain provider-generated selection nodes
    /// in the same AST without fabricating an enabled configuration rule.
    pub fn linked_manual_actions(&self,roots:&[Node])->Result<Vec<Node>,Error> {
        let scenes=self.immediate_scenes.iter().filter(|(id,_)|!self.disabled_immediate_scenes.contains(*id)).map(|(id,s)|(id.clone(),s)).collect();
        let deferred=self.timed_scenes.iter().filter(|(_,s)|s.enabled).map(|(id,_)|id.clone()).collect();
        let mut actions=homesense_core::scene_expand::expand_borrowed_with_deactivations(roots,&scenes,&deferred,&self.deactivation_targets(),1024).map_err(|e|invalid(format!("manual immediate scene: {e:?}")))?;
        for variable in homesense_core::rule::variable_references_with_deferred(&actions,&deferred).map_err(invalid)? {
            if !self.variables.contains_key(&variable){return Err(invalid("undeclared manual scene variable"));}
        }
        lower_feedback(&mut actions,&self.feedback_bindings)?;
        Ok(actions)
    }
    pub fn deactivation_targets(&self)->BTreeMap<String,String> {self.scene_deactivations.iter().filter(|(_,l)|l.enabled).map(|(id,l)|(id.clone(),l.target.clone())).collect()}
    pub fn linked_rules_with_deferred(&self)->Result<Vec<Rule>,Error> {
        self.link_rules(&self.immediate_scenes,&self.schedules.iter().map(|s|s.id.clone()).collect(),true)
    }
    fn link_rules(&self,scenes:&BTreeMap<String,homesense_core::scene_expand::Definition>,schedules:&std::collections::BTreeSet<String>,allow_deferred:bool)->Result<Vec<Rule>,Error> {
        let scenes: BTreeMap<_, _> = scenes.iter().filter(|(id,_)| !self.disabled_immediate_scenes.contains(*id)).map(|(id,scene)|(id.clone(),scene.clone())).collect();
        let deferred=if allow_deferred {self.timed_scenes.iter().filter(|(_,s)|s.enabled).map(|(id,_)|id.clone()).collect()} else {Default::default()};
        let deactivation_targets=if allow_deferred{self.deactivation_targets()}else{Default::default()};
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
            let mut actions = homesense_core::scene_expand::expand_with_deactivations(&rule.actions, &scenes, &deferred, &deactivation_targets, 1024)
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
            Condition::MotorState(_)=>{},
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

impl Document {
    /// Pure preflight, repeated inside the publication transaction. An explicit
    /// migration requires an existing persistent source at the stated revision.
    pub fn published_variable_value(&self, previous: Option<u64>, name: &str, old: Option<(ValueType,Option<&Value>)>) -> Result<Option<Value>,Error> {
        let target=self.variables.get(name).ok_or_else(||invalid("unknown variable"))?;
        if target.lifetime!=Lifetime::Persistent {return Err(invalid("persistent target required"));}
        if let Some(migration)=self.variable_migrations.get(name) {
            let (kind,value)=old.ok_or_else(||invalid("migration source missing"))?;
            if previous!=Some(migration.source_revision) || kind!=migration.source_type {return Err(invalid("migration source changed"));}
            migration.conversion.apply(kind,target.value_type,value).map_err(|_|invalid("variable conversion rejected"))
        } else {
            match old {
                Some((kind,value)) if kind==target.value_type=>Ok(value.cloned()),
                Some(_)=>Err(invalid("explicit variable migration required")),
                None=>Ok(None),
            }
        }
    }
}
