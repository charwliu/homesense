//! Disabled event fragments, not executable automation documents.
use homesense_core::{Command,LevelTransition,rule::{Action,Condition,Expr,Node,Value}};
use serde_json::{Value as Json,json};
fn expression(value:&Expr)->Json {
    match value {
        Expr::Literal(Value::Bool(value))=>json!({"kind":"bool","value":value}),
        Expr::Literal(Value::Integer(value))=>json!({"kind":"integer","value":value}),
        Expr::Variable(id)=>json!({"kind":"variable","id":id}),
        Expr::DeviceOn(device)=>json!({"kind":"device_on","device":device.as_str()}),
        Expr::DeviceLevel(device)=>json!({"kind":"device_level","device":device.as_str()}),
        Expr::Add(left,right)=>json!({"kind":"add","left":expression(left),"right":expression(right)}),
        Expr::Subtract(left,right)=>json!({"kind":"subtract","left":expression(left),"right":expression(right)}),
    }
}
fn condition(value:&Condition)->Json {
    match value {
        Condition::Equal(left,right)=>json!({"kind":"equal","left":expression(left),"right":expression(right)}),
        Condition::Less(left,right)=>json!({"kind":"less","left":expression(left),"right":expression(right)}),
        Condition::Not(value)=>json!({"kind":"not","condition":condition(value)}),
        Condition::All(values)=>json!({"kind":"all","conditions":values.iter().map(condition).collect::<Vec<_>>()}),
        Condition::Any(values)=>json!({"kind":"any","conditions":values.iter().map(condition).collect::<Vec<_>>()}),
        Condition::Unknown(value)=>json!({"kind":"unknown","value":expression(value)}),
        Condition::MotorState(p)=>json!({"kind":"motor_state","device":p.device.as_str(),"state":match p.state{homesense_core::rule_motor::State::Moving=>"moving",homesense_core::rule_motor::State::Stopped=>"stopped"},"evidence_policy":match p.evidence{homesense_core::rule_motor::Evidence::ActualOnly=>"actual_only",homesense_core::rule_motor::Evidence::ReviewedCommandEstimate=>"reviewed_command_estimate"},"max_age_ms":p.max_age_ms,"mapping_review":p.mapping_review,"evidence_review":p.evidence_review}),
    }
}
fn nodes(values:&[Node])->Result<Vec<Json>,String> {
    values.iter().map(|node|{
        let action=match &node.action {
            Action::SceneActivation{..}|Action::RequireQuiescence{..}=>return Err("trusted runtime guard cannot be imported or exported as a source action".into()),
            Action::Motor{device,action,mapping_review}=>json!({"kind":"motor","device":device.as_str(),"action":match action{homesense_core::MotorAction::Open=>"open",homesense_core::MotorAction::Close=>"close",homesense_core::MotorAction::Stop=>"stop"},"mapping_review":mapping_review}),
            Action::Scene{id,activate}=>json!({"kind":"scene","id":id,"activate":activate}),
            Action::Set{variable,value}=>json!({"kind":"set","variable":variable,"value":expression(value)}),
            Action::If{condition:value,then_actions,else_actions}=>json!({"kind":"if","condition":condition(value),"then_actions":nodes(then_actions)?,"else_actions":nodes(else_actions)?}),
            Action::Command{device,command}=>{
                let command=match command {
                    Command::Switch(on)=>json!({"kind":"switch","on":on}),
                    Command::SetLevel{level,transition}=>json!({"kind":"level","value":level.get(),"ramp":*transition==LevelTransition::Ramp}),
                    Command::PanelLed(on)=>json!({"kind":"panel_led","on":on}),
                    _=>return Err("unsupported imported command export".into()),
                };
                json!({"kind":"command","device":device.as_str(),"command":command})
            }
        };
        Ok(json!({"source":node.source,"action":action}))
    }).collect()
}
pub fn draft(event:&crate::semantic::CompiledEvent)->Result<Json,String> {
    Ok(json!({
        "format":"homesense-event-draft-v1",
        "automatic_execution":false,"enabled":false,
        "review_required":["device bindings","trigger","variables","referenced scenes","physical semantics","motor predicate evidence","motor direction calibration","original guarded behavior"],
        "actions":nodes(&event.actions)?,
        "timed_scenes":event.timed_scenes.values().collect::<Vec<_>>()
    }))
}
