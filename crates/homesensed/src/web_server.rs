//! Explicit loopback HTTP listener. LAN exposure requires a separate TLS design;
//! this entry point refuses non-loopback addresses and forwarded host headers.
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
    mut stop: tokio::sync::watch::Receiver<bool>,
) -> Result<(), String> {
    if !address.ip().is_loopback()
        || address.port() == 0
        || api.origin != format!("http://{address}")
        || api.runtime.read().map_err(|_|"management runtime lock poisoned")?.as_ref().is_none_or(|r|r.scenes.len()>64)
    {
        return Err("loopback address, matching origin and at most 64 scenes required".into());
    }
    let listener = tokio::net::TcpListener::bind(address)
        .await
        .map_err(|e| e.to_string())?;
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
                let api=api.clone();let event_permits=event_permits.clone();let event_stop=stop.clone();
                tasks.spawn(async move {
                    let _permit=permit;
                    let service=service_fn(move |request:Request<Incoming>| {
                        let api=api.clone();let event_permits=event_permits.clone();let event_stop=event_stop.clone();
                        async move {
                            if request.uri().path()=="/api/v1/events" {return Ok::<_,std::convert::Infallible>(crate::web_events::upgrade(api,request,event_stop,event_permits));}
                            let (parts,body)=request.into_parts();
                            let processing_timeout=if parts.uri.path().starts_with("/api/v1/config/activations/"){40}else{8};
                            let limit=if parts.uri.path().starts_with("/api/v1/config/drafts/"){crate::web_drafts::MAX_BYTES}else{4096};
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
