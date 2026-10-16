//! Manual keypad scenes keep the established HTTP causal owner and receipts.
use serde_json::{Value,json};
use std::time::Duration;
use tokio::io::{AsyncReadExt,AsyncWriteExt};
use uuid::Uuid;
const SECRET:&str="synthetic-scene-execute-http-0123456789";
async fn http(
    address: std::net::SocketAddr,
    token: &str,
    path: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
    let method = if body.is_some() { "POST" } else { "GET" };
    let body = body.map(|v| v.to_string()).unwrap_or_default();
    stream.write_all(format!("{method} {path} HTTP/1.1\r\nHost: {address}\r\nOrigin: http://{address}\r\nAuthorization: Bearer {token}\r\nIdempotency-Key: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",Uuid::new_v4(),body.len()).as_bytes()).await.unwrap();
    let mut bytes = vec![];
    tokio::time::timeout(Duration::from_secs(3), stream.read_to_end(&mut bytes))
        .await
        .unwrap()
        .unwrap();
    let text = String::from_utf8(bytes).unwrap();
    let (header, body) = text.split_once("\r\n\r\n").unwrap();
    (
        header.split_whitespace().nth(1).unwrap().parse().unwrap(),
        serde_json::from_str(body).unwrap(),
    )
}


#[tokio::test]
async fn scene_execute_manual_unknown_selection_branch_feedback_variable_and_duplicate_receipt(){
    let root=std::env::temp_dir().join(format!("scene-execute-http-{}",Uuid::new_v4()));let r=std::net::TcpListener::bind("127.0.0.1:0").unwrap();let address=r.local_addr().unwrap();drop(r);
    let config=serde_json::to_vec(&json!({"schema_version":2,"revision":1,"gateways":[{"id":"bus","endpoint":"127.0.0.1:1","modules":[{"id":"relay","address":"03","profile":"r8i8_din_pro_v2_7_7","verification":{"status":"simulated"}},{"id":"panel-module","address":"41","profile":"ts6_86_rs485_v2_6_5_6key","verification":{"status":"simulated"},"led_mode":"manual"}]}],"devices":[{"id":"light","name":"Synthetic light","module_id":"relay","channel":1,"enabled":true}],"keypads":[{"id":"panel","name":"Synthetic manual panel","module_id":"panel-module","enabled":true,"keys":[{"number":1,"label":"Manual execute","led_endpoint":{"id":"key1","name":"Indicator","enabled":true},"action":{"kind":"scene_execute","scene":"manual","review_evidence":"synthetic"}}]}]})).unwrap();
    let branch=|on:bool|vec![json!({"source":if on{"else/light"}else{"then/light"},"action":{"kind":"command","device":"light","command":{"kind":"switch","on":on}}}),json!({"source":if on{"else/variable"}else{"then/variable"},"action":{"kind":"set","variable":"mode","value":{"kind":"integer","value":if on{1}else{0}}}}),json!({"source":if on{"else/led"}else{"then/led"},"action":{"kind":"command","device":"key1","command":{"kind":"panel_led","on":on}}})];
    let rules=serde_json::to_vec(&json!({"schema_version":1,"configuration_revision":1,"variables":[{"id":"mode","kind":"integer","lifetime":"persistent"}],"rules":[],"immediate_scenes":[{"id":"manual","enabled":true,"review_evidence":"synthetic manual explicit branches","activate":[{"source":"manual/if","action":{"kind":"if","condition":{"kind":"equal","left":{"kind":"device_on","device":"light"},"right":{"kind":"bool","value":true}},"then_actions":branch(false),"else_actions":branch(true)}}]}]})).unwrap();
    let directory=root.clone();let server=tokio::spawn(async move{homesensed::service::serve_web_simulation(&config,&rules,&directory,Duration::from_secs(6),homesensed::service::WebSimulation{address,secret:SECRET.into(),administrator:true,tls:None}).await});
    for _ in 0..200{if tokio::net::TcpStream::connect(address).await.is_ok(){break;}tokio::time::sleep(Duration::from_millis(10)).await;}
    let (code,devices)=http(address,SECRET,"/api/v1/devices",None).await;assert_eq!(code,200,"{devices}");let panel=devices["devices"].as_array().unwrap().iter().find(|d|d["id"]=="panel").unwrap();assert_eq!(panel["keys"][0]["action"],json!({"kind":"scene_execute","scene":"manual","path":"/api/v1/scene-actions"}));assert!(panel["keys"][0]["card_action"].is_null());
    let (code,scenes)=http(address,SECRET,"/api/v1/scenes",None).await;assert_eq!(code,200);let scene=scenes["scenes"].as_array().unwrap().iter().find(|s|s["id"]=="manual").unwrap();assert!(scene["selected"].is_null());assert_eq!(scene["can_control"],true);
    let mut receipts=vec![];
    for expected in [2,4]{let body=json!({"scene":"manual","on":true,"request_id":Uuid::new_v4().to_string()});let (code,accepted)=http(address,SECRET,"/api/v1/scene-actions",Some(body.clone())).await;assert_eq!(code,202,"{accepted}");assert_eq!(accepted["receipt"]["commands"].as_array().unwrap().len(),2);receipts.push(accepted.clone());
        tokio::time::timeout(Duration::from_secs(2),async{loop{let sql=rusqlite::Connection::open(root.join("automation-simulation.sqlite")).unwrap();let n=sql.query_row("SELECT count(*) FROM command_journal WHERE status='written'",[],|r|r.get::<_,i64>(0)).unwrap();drop(sql);if n==expected{break;}tokio::time::sleep(Duration::from_millis(10)).await;}}).await.unwrap();
        let (code,replayed)=http(address,SECRET,"/api/v1/scene-actions",Some(body)).await;assert_eq!(code,200);assert_eq!(replayed,accepted);tokio::time::sleep(Duration::from_millis(50)).await;
    }
    server.await.unwrap().unwrap();let sql=rusqlite::Connection::open(root.join("automation-simulation.sqlite")).unwrap();assert_eq!(sql.query_row("SELECT count(*) FROM command_journal",[],|r|r.get::<_,i64>(0)).unwrap(),4);assert_eq!(sql.query_row("SELECT version FROM logical_variable WHERE name='mode'",[],|r|r.get::<_,i64>(0)).unwrap(),2);assert_eq!(sql.query_row("SELECT count(*) FROM scene_observation",[],|r|r.get::<_,i64>(0)).unwrap(),0);assert_eq!(receipts.len(),2);drop(sql);std::fs::remove_dir_all(root).unwrap();
}
