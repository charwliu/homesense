//! Explicit loopback HTTP or private-LAN HTTPS; no plaintext LAN fallback.
use crate::web_api::Api;
use http_body_util::{BodyExt, Full, Limited};
use hyper::{
    Request, Response, StatusCode,
    body::{Bytes, Incoming},
    service::service_fn,
};
use std::{net::SocketAddr, sync::Arc, time::Duration};
pub async fn serve(
    api: Arc<Api>,
    address: SocketAddr,
    stop: tokio::sync::watch::Receiver<bool>,
) -> Result<(), String> {serve_with_tls(api,address,stop,None).await}
pub async fn serve_with_tls(api:Arc<Api>,address:SocketAddr,stop:tokio::sync::watch::Receiver<bool>,tls:Option<crate::web_tls::Settings>)->Result<(),String> {serve_ready(api,address,stop,tls,None).await}
pub(crate) async fn serve_ready(api:Arc<Api>,address:SocketAddr,mut stop:tokio::sync::watch::Receiver<bool>,tls:Option<crate::web_tls::Settings>,ready:Option<tokio::sync::oneshot::Sender<()>>)->Result<(),String> {
    if let Some(tls)=&tls {tls.validate_address(address)?;if api.origin!=tls.origin{return Err("TLS origin mismatch".into());}}

    if (tls.is_none() && (!address.ip().is_loopback() || api.origin != format!("http://{address}")))
        || address.port() == 0
        || api.runtime.read().map_err(|_|"management runtime lock poisoned")?.as_ref().is_none_or(|r|crate::web_api::check_catalog_capacity(r.scenes.len()).is_err())
    {
        return Err("valid listener address, matching origin and bounded scene catalog required".into());
    }
    let listener = tokio::net::TcpListener::bind(address)
        .await
        .map_err(|e| e.to_string())?;
    if let Some(ready)=ready {let _=ready.send(());}
    let event_permits=Arc::new(tokio::sync::Semaphore::new(16));
    let permits = Arc::new(tokio::sync::Semaphore::new(32));
    let mut tasks = tokio::task::JoinSet::new();
    while !*stop.borrow() {
        tokio::select! {
            changed=stop.changed()=>{if changed.is_err() || *stop.borrow(){break;}},
            Some(_)=tasks.join_next(),if !tasks.is_empty()=>{},
            accepted=listener.accept()=>{
                let (stream,_)=accepted.map_err(|e|e.to_string())?;
                let Ok(permit)=permits.clone().try_acquire_owned() else {continue};
                let api=api.clone();let event_permits=event_permits.clone();let event_stop=stop.clone();let tls=tls.clone();
                tasks.spawn(async move {
                    let _permit=permit;
                    trait Stream:tokio::io::AsyncRead+tokio::io::AsyncWrite+Unpin+Send {}
                    impl<T:tokio::io::AsyncRead+tokio::io::AsyncWrite+Unpin+Send> Stream for T {}
                    let stream:Box<dyn Stream>=if let Some(tls)=tls {
                        match tokio::time::timeout(Duration::from_secs(3),tls.acceptor.accept(stream)).await {Ok(Ok(stream))=>Box::new(stream),_=>return}
                    }else{Box::new(stream)};
                    let service=service_fn(move |request:Request<Incoming>| {
                        let api=api.clone();let event_permits=event_permits.clone();let event_stop=event_stop.clone();
                        async move {
                            if request.uri().path()=="/api/v1/events" {return Ok::<_,std::convert::Infallible>(crate::web_events::upgrade(api,request,event_stop,event_permits));}
                            let (parts,body)=request.into_parts();
                            let processing_timeout=if parts.uri.path().starts_with("/api/v1/config/activations/"){40}else{8};
                            let whole_draft=matches!(crate::web_management_receipts::Operation::route(&parts.method,parts.uri.path()),
                                Ok(Some(crate::web_management_receipts::Operation::Draft(_,crate::web_management_receipts::DraftOperation::Save))));
                            let limit=if whole_draft{crate::web_drafts::MAX_BYTES}
                                else if parts.uri.path().starts_with("/api/v1/config/drafts/"){crate::web_drafts::MAX_EDIT_BYTES}else{4096};
                            let response=match tokio::time::timeout(Duration::from_secs(3),Limited::new(body,limit).collect()).await {
                                Ok(Ok(body))=>match tokio::time::timeout(Duration::from_secs(processing_timeout),api.handle(Request::from_parts(parts,body.to_bytes()))).await {
                                    Ok(response)=>response,
                                    Err(_)=>failure(StatusCode::SERVICE_UNAVAILABLE)
                                },
                                _=>failure(StatusCode::PAYLOAD_TOO_LARGE)
                            };
                            Ok::<_,std::convert::Infallible>(response)
                        }
                    });
                    let mut builder=hyper::server::conn::http1::Builder::new();
                    builder.max_buf_size(16384);
                    let _=tokio::time::timeout(Duration::from_secs(45),builder.serve_connection(hyper_util::rt::TokioIo::new(stream),service).with_upgrades()).await;
                });
            }
        }
    }
    tasks.abort_all();
    while tasks.join_next().await.is_some() {}
    Ok(())
}
fn failure(code: StatusCode) -> Response<Full<Bytes>> {
    Response::builder()
        .status(code)
        .header("content-type", "application/json")
        .header("cache-control", "no-store")
        .body(Full::new(Bytes::from_static(
            b"{\"error\":\"request_unavailable\",\"retry\":false}",
        )))
        .unwrap()
}

#[cfg(test)]
mod large_draft_tests {
    use super::*;
    use crate::{admission::{Admission,Options},automation_runtime::{AutomationRuntime,StartupPolicy},
        web_auth::{Grant,Role,Tokens},web_drafts::Drafts,web_receipts::Receipts};
    use homesense_core::ExecutionContext;
    use serde_json::{Value,json};
    use std::sync::{Mutex,RwLock};
    use tokio::io::{AsyncReadExt,AsyncWriteExt};
    use uuid::Uuid;

    async fn request(address:SocketAddr,method:&str,path:&str,body:&[u8],secret:&str)->(u16,Vec<u8>) {
        let stream=tokio::net::TcpStream::connect(address).await.unwrap();
        let(mut read,mut write)=stream.into_split();
        let headers=format!("{method} {path} HTTP/1.1\r\nHost: {address}\r\nOrigin: http://{address}\r\nAuthorization: Bearer {secret}\r\nContent-Type: application/json\r\nIdempotency-Key: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",Uuid::new_v4(),body.len());
        let sender=async {write.write_all(headers.as_bytes()).await?;write.write_all(body).await};
        let receiver=async {let mut bytes=vec![];let result=read.read_to_end(&mut bytes).await;(result,bytes)};
        let(sent,(received,bytes))=tokio::join!(sender,receiver);
        let split=bytes.windows(4).position(|part|part==b"\r\n\r\n").expect("complete HTTP response headers");
        let status=std::str::from_utf8(&bytes[..split]).unwrap().split_whitespace().nth(1).unwrap().parse().unwrap();
        if status==200 {sent.unwrap();received.unwrap();}
        (status,bytes[split+4..].to_vec())
    }

    #[tokio::test]
    async fn large_configuration_http_roundtrip_keeps_control_edit_and_owner_boundaries() {
        let root=std::env::temp_dir().join(format!("large-http-draft-{}",Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let devices:Value=serde_json::from_slice(include_bytes!("../../../config/simulation.json")).unwrap();
        let initial=json!({"schema_version":1,"configuration_revision":1,"variables":[],"rules":[]});
        homesense_storage::Store::open(root.join("runtime.sqlite")).unwrap().publish_automation(None,
            &serde_json::to_vec(&devices).unwrap(),&serde_json::to_vec(&initial).unwrap(),ExecutionContext::Simulation,&[]).unwrap();
        let caller=Uuid::new_v4();
        let actor=Admission::open(root.join("runtime.sqlite"),Options{context:ExecutionContext::Simulation,..Default::default()},move|id|id==caller).await.unwrap();
        actor.synchronized(1,1).await.unwrap();
        let runtime=AutomationRuntime::load_with_startup(actor.clone(),caller,1_800_000_000_000,1000,8,Default::default(),StartupPolicy::Suppress).await.unwrap();
        let listener=std::net::TcpListener::bind("127.0.0.1:0").unwrap();let address=listener.local_addr().unwrap();drop(listener);
        let tokens=Arc::new(Tokens::default());
        let admin="synthetic-large-admin-01234567890123456789";
        let member="synthetic-large-member-01234567890123456789";
        for(secret,role)in[(admin,Role::Administrator),(member,Role::Member)] {
            tokens.insert(Uuid::new_v4(),secret,Grant{controller:caller,role,devices:["simulation:light".into()].into(),scenes:Default::default()}).unwrap();
        }
        let api=Arc::new(Api{daily_backup:Default::default(),hap_placement:Default::default(),hap_bridges:Default::default(),hap_status:Default::default(),backup_source:None,publisher:None,
            origin:format!("http://{address}"),tokens,runtime:RwLock::new(Some(crate::web_api::Runtime{control:runtime.control(),revision:1,boot:actor.boot_id().into(),scenes:Default::default()})),
            drafts:Some((ExecutionContext::Simulation,Mutex::new(Drafts::open(&root.join("drafts.sqlite")).unwrap()))),
            receipts:Mutex::new(Receipts::open(&root.join("receipts.sqlite")).unwrap())});
        let(stop,stopping)=tokio::sync::watch::channel(false);let(ready,started)=tokio::sync::oneshot::channel();
        let server=tokio::spawn(serve_ready(api.clone(),address,stopping,None,Some(ready)));started.await.unwrap();
        let mut next=devices.clone();next["revision"]=2.into();
        let automation=json!({"schema_version":1,"configuration_revision":2,"variables":[],"rules":[],
            "immediate_scenes":(0..384).map(|i|json!({"id":format!("synthetic-disabled-{i}"),"enabled":false,"review_evidence":"x".repeat(8000),"activate":[]})).collect::<Vec<_>>()});
        let body=serde_json::to_vec(&json!({"expected_revision":1,"revision":2,"devices":next,"automation":automation})).unwrap();
        assert!(body.len()>2*1024*1024 && body.len()<crate::web_drafts::MAX_BYTES);
        let id=Uuid::new_v4();let path=format!("/api/v1/config/drafts/{id}");
        let saved=request(address,"POST",&path,&body,admin).await;
        let source=request(address,"GET",&format!("{path}/source"),&[],admin).await;
        let wrong_role=request(address,"POST",&format!("/api/v1/config/drafts/{}",Uuid::new_v4()),&body,member).await;
        let small_edit=request(address,"POST",&format!("{path}/room"),&vec![b' ';256*1024+1],admin).await;
        let ordinary=request(address,"POST","/api/v1/device-commands",&vec![b' ';4097],admin).await;
        let excessive=request(address,"POST",&format!("/api/v1/config/drafts/{}",Uuid::new_v4()),&vec![b' ';crate::web_drafts::MAX_BYTES+1],admin).await;
        stop.send(true).unwrap();server.await.unwrap().unwrap();
        let stored=api.drafts.as_ref().unwrap().1.lock().unwrap().list(caller).unwrap();
        let active=api.runtime.read().unwrap().as_ref().unwrap().revision;
        drop(api);drop(runtime);actor.shutdown().await.unwrap();std::fs::remove_dir_all(root).unwrap();
        assert_eq!(saved.0,200,"large valid draft must pass the actual HTTP body collector");
        assert_eq!(source.0,200);assert!(source.1.len()>2*1024*1024);
        let source:Value=serde_json::from_slice(&source.1).unwrap();
        assert_eq!(serde_json::from_str::<Value>(source["automation"].as_str().unwrap()).unwrap(),automation);
        assert_eq!(wrong_role.0,403);assert_eq!(small_edit.0,413);assert_eq!(ordinary.0,413);assert_eq!(excessive.0,413);
        assert_eq!(stored.len(),1,"rejected requests cannot create any additional draft");
        assert_eq!(active,1,"saving and reading must not publish the draft");
    }
}
