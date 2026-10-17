//! Native API/WSS integration against an in-memory simulator, never a gateway.
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_tungstenite::tungstenite::{Message, client::IntoClientRequest};
use uuid::Uuid;
const SECRET: &str = "synthetic-keypad-actions-test-token-0123456789";
async fn http(
    address: std::net::SocketAddr,
    token: &str,
    path: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let method = if body.is_some() { "POST" } else { "GET" };
    http_method(address,token,path,method,body).await
}
async fn http_method(address:std::net::SocketAddr,token:&str,path:&str,method:&str,body:Option<Value>)->(u16,Value) {
    let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
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
async fn unified_http_is_scoped_idempotent_and_pushes_source_indication() {
    use homesense_core::ExecutionContext;
    let root=std::env::temp_dir().join(format!("keypad-actions-http-{}",Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    let mut config:Value=serde_json::from_slice(include_bytes!("../../../config/test/native-app-keypad/devices.json")).unwrap();
    config["keypads"][0]["keys"][0]["local_indicator"]=json!({"kind":"scene_selection","group":"reading-group","value":1,"selected_color":"red","unselected_color":"green","review_evidence":"synthetic reviewed source LED"});
    let config=serde_json::to_vec(&config).unwrap();
    let mut rules:Value=serde_json::from_slice(include_bytes!("../../../config/test/native-app-keypad/automation.json")).unwrap();
    rules["scene_switch_groups"][0]["reconciliation"]["enabled"]=json!(false);
    let rules=serde_json::to_vec(&rules).unwrap();
    let mut store=homesense_storage::Store::open(root.join("automation-simulation.sqlite")).unwrap();
    store.publish_automation(None,&config,&rules,ExecutionContext::Simulation,&[]).unwrap();
    store.accept_queue_batch_with_variables("seed",&[],&[],0,None,&[homesense_storage::variables::Update{revision:1,name:"selection".into(),expected_version:0,value:Some(homesense_core::rule::Value::Integer(0))}]).unwrap();
    drop(store);
    let reservation=std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address=reservation.local_addr().unwrap();drop(reservation);
    let directory=root.clone();
    let service=tokio::spawn(async move {homesensed::service::serve_web_simulation(&config,&rules,&directory,Duration::from_secs(8),homesensed::service::WebSimulation{address,secret:SECRET.into(),administrator:true,tls:None}).await});
    for _ in 0..200 {if tokio::net::TcpStream::connect(address).await.is_ok(){break;}tokio::time::sleep(Duration::from_millis(10)).await;}
    if service.is_finished(){panic!("simulation failed: {:?}",service.await.unwrap());}
    let (status,session)=http(address,SECRET,"/api/v1/session",None).await;
    assert_eq!(status,200);assert_eq!(session["capabilities"]["keypad_actions"]["version"],1);
    let (_,issued)=http(address,SECRET,"/api/v1/credentials",Some(json!({"role":"Member","devices":["legacy-key-1"],"scenes":["reading"]}))).await;
    let limited=issued["secret"].as_str().unwrap();
    let (_,member)=http(address,SECRET,"/api/v1/credentials",Some(json!({"role":"Member","devices":["legacy-key-1","fixture-light","fixture-downlight"],"scenes":["reading","meeting","off"]}))).await;
    let member_token=member["secret"].as_str().unwrap();
    let input=json!({"revision":"1","keypad":"panel","key":1,"request_id":"source-on","gesture":"activate"});
    let (status,denied)=http(address,limited,"/api/v1/keypad-actions",Some(input.clone())).await;
    assert_eq!(status,403,"{denied}");assert_eq!(denied["request_id"],"source-on");
    let mut forged=input.clone();forged["target"]=json!("fixture-light");
    assert_eq!(http(address,SECRET,"/api/v1/keypad-actions",Some(forged)).await.0,400);
    let mut stale=input.clone();stale["revision"]=json!("2");
    assert_eq!(http(address,SECRET,"/api/v1/keypad-actions",Some(stale)).await.0,409);
    let mut request=format!("ws://{address}/api/v1/events").into_client_request().unwrap();
    request.headers_mut().insert("origin",format!("http://{address}").parse().unwrap());
    request.headers_mut().insert("authorization",format!("Bearer {SECRET}").parse().unwrap());
    let (mut socket,_)=tokio_tungstenite::client_async(request,tokio::net::TcpStream::connect(address).await.unwrap()).await.unwrap();
    socket.send(Message::Text(r#"{"cursor":null}"#.into())).await.unwrap();
    tokio::time::timeout(Duration::from_secs(2),socket.next()).await.unwrap().unwrap().unwrap();
    let (status,accepted)=http(address,member_token,"/api/v1/keypad-actions",Some(input.clone())).await;
    assert_eq!(status,202,"{accepted}");assert_eq!(accepted["outcome"],"accepted");assert_eq!(accepted["execution"]["kind"],"scene");
    let (status,replay)=http(address,member_token,"/api/v1/keypad-actions",Some(input.clone())).await;
    assert_eq!(status,200);assert_eq!(replay,accepted,"same request must replay its persisted receipt");
    let mut conflict=input.clone();conflict["key"]=json!(3);
    assert_eq!(http(address,member_token,"/api/v1/keypad-actions",Some(conflict)).await.0,409);
    let update=tokio::time::timeout(Duration::from_secs(3),async {loop {
        if let Some(Ok(Message::Text(text)))=socket.next().await {
            let state:Value=serde_json::from_str(&text).unwrap();
            let Some(panel)=state["devices"].as_array().and_then(|devices|devices.iter().find(|device|device["id"]=="panel")) else{continue;};
            let key=&panel["keys"][0];
            if key["indication"]["request_id"]=="source-on" && key["indication"]["status"]=="written" {break key.clone();}
        }
    }}).await.unwrap();
    assert_eq!(update["indication"]["color"],"red");assert_eq!(update["indication"]["observed"],false);assert!(update["led"]["observed"].is_null());
    // A permission change retains the credential ID but must not disclose an
    // old accepted receipt or turn that old request into another execution.
    let update=json!({"expected":member["grant"],"role":"Member","devices":["legacy-key-1"],"scenes":["reading"]});
    let (status,result)=http_method(address,SECRET,&format!("/api/v1/credentials/{}",member["id"].as_str().unwrap()),"PATCH",Some(update)).await;
    assert_eq!(status,200,"{result}");
    let (status,stale_scope)=http(address,member_token,"/api/v1/keypad-actions",Some(input)).await;
    assert_eq!(status,409,"{stale_scope}");assert!(stale_scope["execution"].is_null());
    socket.close(None).await.unwrap();
    service.await.unwrap().unwrap();
    let connection=rusqlite::Connection::open(root.join("automation-simulation.sqlite")).unwrap();
    let runs:i64=connection.query_row("SELECT count(*) FROM scene_run",[],|row|row.get(0)).unwrap();
    assert_eq!(runs,1,"duplicate, invalid, stale and denied requests must not create extra executions");
    drop(connection);std::fs::remove_dir_all(root).unwrap();
}
