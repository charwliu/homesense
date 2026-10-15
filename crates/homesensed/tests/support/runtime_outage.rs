//! Synthetic management checks shared by fault-path integration tests.
use serde_json::{Value,json};
use std::{net::SocketAddr,path::Path,time::Duration};
use tokio::io::{AsyncReadExt,AsyncWriteExt};

pub async fn http(address:SocketAddr,token:&str,path:&str,body:Option<Value>)->Result<(u16,Value),String> {
    let mut socket=tokio::net::TcpStream::connect(address).await.map_err(|e|e.to_string())?;
    let method=if body.is_some(){"POST"}else{"GET"};let body=body.map_or(String::new(),|v|v.to_string());
    socket.write_all(format!("{method} {path} HTTP/1.1\r\nHost: {address}\r\nOrigin: http://{address}\r\nAuthorization: Bearer {token}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.map_err(|e|e.to_string())?;
    let mut bytes=vec![];
    tokio::time::timeout(Duration::from_secs(4),socket.read_to_end(&mut bytes)).await.map_err(|e|e.to_string())?.map_err(|e|e.to_string())?;
    let text=String::from_utf8(bytes).map_err(|e|e.to_string())?;
    let (header,body)=text.split_once("\r\n\r\n").ok_or("HTTP response missing")?;
    Ok((header.split_whitespace().nth(1).ok_or("HTTP status missing")?.parse::<u16>().map_err(|e|e.to_string())?,serde_json::from_str(body).map_err(|e|e.to_string())?))
}
pub async fn wait_unavailable(address:SocketAddr,token:&str,revision:&str,service:&tokio::task::JoinHandle<Result<(),String>>) {
    tokio::time::timeout(Duration::from_secs(3),async {loop {
        assert!(!service.is_finished(),"transport failure ended the established service early");
        if let Ok((200,state))=http(address,token,"/api/v1/diagnostics",None).await
            && state["revision"]==revision && state["gateway_ready"]==false && state["ready"]==false {break;}
        tokio::time::sleep(Duration::from_millis(10)).await;
    }}).await.unwrap();
}
pub async fn assert_gated(address:SocketAddr,token:&str,revision:&str,device:&str) {
    let (code,state)=http(address,token,"/api/v1/diagnostics",None).await.unwrap();
    assert_eq!(code,200);assert_eq!(state["revision"],revision);
    assert_eq!(state["gateway_ready"],false);assert_eq!(state["ready"],false);
    assert_eq!(http(address,token,"/health/ready",None).await.unwrap().0,503);
    assert_eq!(http(address,token,"/api/v1/devices",None).await.unwrap().0,503);
    let (code,receipt)=http(address,token,"/api/v1/device-commands",Some(json!({"device":device,"revision":revision,"request_id":uuid::Uuid::new_v4().to_string(),"command":{"action":"brightness","level":80}}))).await.unwrap();
    // A runtime error is conservatively unknown, not proof of acceptance or
    // rejection. The caller separately proves no journal entry or wire output.
    assert_eq!(code,202);assert_eq!(receipt["outcome"],"unknown");assert_eq!(receipt["retry"],false);
}
pub fn assert_empty_journal(directory:&Path) {
    let db=rusqlite::Connection::open(directory.join("automation-tcp.sqlite")).unwrap();
    for table in ["command_journal","scene_run"] {
        let count:i64=db.query_row(&format!("SELECT count(*) FROM {table}"),[],|row|row.get(0)).unwrap();
        assert_eq!(count,0,"unexpected actuator admission or replay in {table}");
    }
}
