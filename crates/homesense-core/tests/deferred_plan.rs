use homesense_core::{Command,DeviceId,rule::*,scene::Step,deferred_plan};
#[test]
fn delayed_outputs_reserve_budget_and_keep_variable_plan_and_call_order() {
    let variables=[("mode".into(),Variable{value_type:ValueType::Integer,lifetime:Lifetime::Persistent,value:Some(Value::Integer(0))})].into();
    let nodes=vec![
        Node{source:"set".into(),action:Action::Set{variable:"mode".into(),value:Expr::Literal(Value::Integer(1))}},
        Node{source:"call1".into(),action:Action::Scene{id:"later".into(),activate:true}},
        Node{source:"call2".into(),action:Action::Scene{id:"later".into(),activate:true}},
    ];
    let definitions=[("later".into(),vec![Step{device:DeviceId::new("light").unwrap(),command:Command::Switch(true),delay_ms:3000,source:"member".into()}])].into();
    let plan=evaluate(&nodes,&variables,Budget::default()).unwrap();
    let required=plan.action_cost+2;
    assert_eq!(deferred_plan::reserve(plan,&definitions,required-1).unwrap_err(),deferred_plan::Error::Budget);
    let reserved=deferred_plan::reserve(evaluate(&nodes,&variables,Budget::default()).unwrap(),&definitions,required).unwrap();
    assert_eq!(reserved.action_cost,required);
    assert_eq!(reserved.invocations.iter().map(|i|i.action_index).collect::<Vec<_>>(),vec![1,2]);
    assert_eq!(reserved.invocations[0].steps[0].source,"call1 -> member");
    assert_eq!(reserved.invocations[1].steps[0].delay_ms,3000);
    assert_eq!(reserved.plan.variables["mode"].value,Some(Value::Integer(1)));
    assert_eq!(variables["mode"].value,Some(Value::Integer(0)));
    assert!(reserved.plan.commands.is_empty());
}
