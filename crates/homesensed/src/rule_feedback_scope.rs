//! Bounded over-approximation of rule input ports. This is a read optimization,
//! never the evaluator's actual read set or a source of execution authority.
use homesense_core::{DeviceId, rule::{Action, Condition, Expr, Node}};
use std::collections::BTreeSet;

pub(super) fn inputs(nodes:&[Node])->Option<BTreeSet<DeviceId>> {
    let mut scan=Scan{remaining:65_536,ids:BTreeSet::new()};
    scan.nodes(nodes,0).then_some(scan.ids)
}
struct Scan {remaining:usize,ids:BTreeSet<DeviceId>}
impl Scan {
    fn spend(&mut self,depth:usize)->bool {
        if depth>64 || self.remaining==0 {return false;}
        self.remaining-=1;true
    }
    fn expression(&mut self,value:&Expr,depth:usize)->bool {
        if !self.spend(depth) {return false;}
        match value {
            Expr::DeviceOn(id)|Expr::DeviceLevel(id)=>{self.ids.insert(id.clone());true},
            Expr::Add(a,b)|Expr::Subtract(a,b)=>self.expression(a,depth+1)&&self.expression(b,depth+1),
            Expr::Literal(_)|Expr::Variable(_)=>true,
        }
    }
    fn condition(&mut self,value:&Condition,depth:usize)->bool {
        if !self.spend(depth) {return false;}
        match value {
            Condition::Equal(a,b)|Condition::Less(a,b)=>self.expression(a,depth+1)&&self.expression(b,depth+1),
            Condition::Unknown(value)=>self.expression(value,depth+1),
            Condition::Not(value)=>self.condition(value,depth+1),
            Condition::All(values)|Condition::Any(values)=>values.iter().all(|v|self.condition(v,depth+1)),
            Condition::MotorState(_)=>true,
        }
    }
    fn nodes(&mut self,nodes:&[Node],depth:usize)->bool {
        nodes.iter().all(|node| {
            if !self.spend(depth) {return false;}
            match &node.action {
                Action::Set{value,..}=>self.expression(value,depth+1),
                Action::If{condition,then_actions,else_actions}=>self.condition(condition,depth+1)
                    &&self.nodes(then_actions,depth+1)&&self.nodes(else_actions,depth+1),
                _=>true,
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use homesense_core::{Command,rule::Value};
    #[test]
    fn includes_nested_predicates_and_both_branches_but_not_command_targets() {
        let id=|name|DeviceId::new(name).unwrap();
        let set=|name,value|Node{source:"synthetic".into(),action:Action::Set{variable:name,value}};
        let nodes=vec![Node{source:"synthetic".into(),action:Action::If {
            condition:Condition::All(vec![Condition::Unknown(Expr::DeviceOn(id("supply"))),
                Condition::Not(Box::new(Condition::Less(Expr::DeviceLevel(id("level")),Expr::Literal(Value::Integer(20)))))]),
            then_actions:vec![set("a".into(),Expr::Add(Box::new(Expr::DeviceLevel(id("then"))),Box::new(Expr::Literal(Value::Integer(1)))))],
            else_actions:vec![set("b".into(),Expr::Subtract(Box::new(Expr::DeviceLevel(id("else"))),Box::new(Expr::Variable("x".into())))),
                Node{source:"synthetic".into(),action:Action::Command{device:id("write-only"),command:Command::Switch(false)}}],
        }}];
        assert_eq!(inputs(&nodes),Some(["supply","level","then","else"].map(id).into()));
    }
    #[test]
    fn scan_exhaustion_falls_back_without_changing_the_evaluator_budget() {
        let mut condition=Condition::Unknown(Expr::Literal(Value::Bool(true)));
        for _ in 0..65 {condition=Condition::Not(Box::new(condition));}
        let node=Node{source:"synthetic".into(),action:Action::If{condition,then_actions:vec![],else_actions:vec![]}};
        assert!(inputs(&[node]).is_none());
        let node=Node{source:"synthetic".into(),action:Action::Command{device:DeviceId::new("unused").unwrap(),command:Command::Switch(false)}};
        assert!(inputs(&vec![node;65_537]).is_none());
    }
}
