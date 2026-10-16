//! Device-card motor intent against the actual native HTTP owner and simulator.
use serde_json::{Value,json};
use std::time::Duration;
use tokio::io::{AsyncReadExt,AsyncWriteExt};
use uuid::Uuid;
const SECRET:&str="synthetic-device-motor-token-0123456789";
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

fn count(root:&std::path::Path)->i64 {rusqlite::Connection::open(root.join("automation-simulation.sqlite")).unwrap().query_row("SELECT count(*) FROM command_journal",[],|r|r.get(0)).unwrap()}
async fn settled(root:&std::path::Path,n:i64) {
    tokio::time::timeout(Duration::from_secs(3),async {loop{let sql=rusqlite::Connection::open(root.join("automation-simulation.sqlite")).unwrap();let ready=sql.query_row("SELECT count(*) FROM command_journal WHERE status='written'",[],|r|r.get::<_,i64>(0)).unwrap();drop(sql);if ready==n{break;}tokio::time::sleep(Duration::from_millis(10)).await;}}).await.unwrap();
}
#[tokio::test]
async fn motor_press_http_revision_scope_idempotency_absolute_paths_and_zero_queue_rejections(){
    let root=std::env::temp_dir().join(format!("motor-native-{}",Uuid::new_v4()));
    let reservation=std::net::TcpListener::bind("127.0.0.1:0").unwrap();let address=reservation.local_addr().unwrap();drop(reservation);
    let config=serde_json::to_vec(&json!({"schema_version":2,"revision":1,"gateways":[{"id":"bus","endpoint":"127.0.0.1:1","modules":[{"id":"motor","address":"22","profile":"c4_din_pro_v2_7_7","verification":{"status":"simulated"}},{"id":"relay","address":"20","profile":"r8i8_din_pro_v2_7_7","verification":{"status":"simulated"}}]}],"devices":[{"id":"curtain","name":"Synthetic curtain","module_id":"motor","channel":1,"enabled":true,"motor_calibration":{"open_direction":"down","stop_verified":true,"evidence":"synthetic","verified_at_unix_ms":1},"curtain_state":{"open_travel_ms":10000,"close_travel_ms":10000,"stop_led_idle_ms":1,"evidence":"synthetic","reviewed_at_unix_ms":1}},{"id":"light","name":"Synthetic relay","module_id":"relay","channel":1,"enabled":true}]})).unwrap();
    let directory=root.clone();let service=tokio::spawn(async move {homesensed::service::serve_web_simulation(&config,br#"{"schema_version":1,"configuration_revision":1,"variables":[],"rules":[]}"#,&directory,Duration::from_secs(8),homesensed::service::WebSimulation{address,secret:SECRET.into(),administrator:true,tls:None}).await});
    for _ in 0..200 {if tokio::net::TcpStream::connect(address).await.is_ok(){break;}tokio::time::sleep(Duration::from_millis(10)).await;}
    let (code,issued)=http(address,SECRET,"/api/v1/credentials",Some(json!({"role":"Member","devices":["light"],"scenes":[]}))).await;assert_eq!(code,201,"{issued}");let limited=issued["secret"].as_str().unwrap();
    let body=|device:&str,command:Value|json!({"device":device,"revision":"1","request_id":Uuid::new_v4().to_string(),"command":command});
    let press=json!({"action":"motor_press","direction":"open"});
    let mut missing=body("curtain",press.clone());missing.as_object_mut().unwrap().remove("revision");
    let mut stale=body("curtain",press.clone());stale["revision"]=json!("2");
    for (token,path,request,expected) in [
        (SECRET,"/api/v1/device-commands",missing,400),
        (SECRET,"/api/v1/device-commands",stale,409),
        (SECRET,"/api/v1/device-commands",body("curtain",json!({"action":"motor_press","direction":"sideways"})),400),
        (SECRET,"/api/v1/device-commands",body("light",press.clone()),400),
        (limited,"/api/v1/device-commands",body("curtain",press.clone()),403),
        (SECRET,"/api/v1/module-commands",json!({"module_id":"motor","channel":1,"revision":"1","request_id":Uuid::new_v4().to_string(),"command":press}),400),
    ]{let (code,value)=http(address,token,path,Some(request)).await;assert_eq!(code,expected,"{value}");assert_eq!(count(&root),0,"rejected intent must never enter journal");}
    let first=body("curtain",json!({"action":"motor_press","direction":"open"}));
    let (code,accepted)=http(address,SECRET,"/api/v1/device-commands",Some(first.clone())).await;assert_eq!(code,202,"{accepted}");settled(&root,1).await;
    let (code,repeated)=http(address,SECRET,"/api/v1/device-commands",Some(first)).await;assert_eq!(code,200,"{repeated}");assert_eq!(accepted,repeated);assert_eq!(count(&root),1,"same receipt cannot normalize again into STOP");
    for (n,action) in [json!({"action":"motor_press","direction":"open"}),json!({"action":"open"}),json!({"action":"open"}),json!({"action":"stop"})].into_iter().enumerate(){let (code,value)=http(address,SECRET,"/api/v1/device-commands",Some(body("curtain",action))).await;assert_eq!(code,202,"{value}");settled(&root,n as i64+2).await;}
    service.await.unwrap().unwrap();
    let sql=rusqlite::Connection::open(root.join("automation-simulation.sqlite")).unwrap();let intents=sql.prepare("SELECT hex(intent) FROM command_journal ORDER BY command_id").unwrap().query_map([],|r|r.get::<_,String>(0)).unwrap().collect::<Result<Vec<_>,_>>().unwrap();
    assert_eq!(intents[0],intents[2]);assert_eq!(intents[2],intents[3]);assert_eq!(intents[1],intents[4]);assert_ne!(intents[0],intents[1]);drop(sql);std::fs::remove_dir_all(root).unwrap();
}
