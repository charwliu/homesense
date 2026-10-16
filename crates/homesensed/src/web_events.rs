//! Authenticated, bounded WebSocket state recovery. No event drives an action.
use crate::{
    automation_control::{Action, Reply},
    management_events::Cursor,
    web_api::Api,
    web_auth::Grant,
};
use futures_util::{SinkExt, StreamExt};
use http_body_util::{BodyExt, Full};
use hyper::{
    Request, Response, StatusCode,
    body::{Bytes, Incoming},
};
use serde_json::json;
use std::{sync::Arc, time::Duration};
use tokio_tungstenite::{
    WebSocketStream,
    tungstenite::{
        Message,
        protocol::{Role, WebSocketConfig},
    },
};
/// Software projection semantics are independent of the physical event cursor.
/// Ignore observation timestamps, versions and lease counters so unchanged
/// repeated readbacks do not cause another state frame.
pub(crate) fn light_projection(snapshot: &crate::management_devices::DeviceSnapshot) -> serde_json::Value {
    json!(snapshot.devices.iter()
        .filter(|view| view.state.kind == homesense_core::ModuleKind::Dimmer)
        .map(|view| {
            let reported = match view.reported {
                Some(homesense_core::Observation::Level(level)) => json!({"level":level.get(),"on":level.get()>0}),
                Some(homesense_core::Observation::Switch(on)) => json!({"on":on}),
                _ => serde_json::Value::Null,
            };
            (view.id.as_str(), view.recorded, reported)
        }).collect::<Vec<_>>())
}
fn current_grant(api: &Api, headers: &hyper::HeaderMap) -> Option<Grant> {
    if headers.get_all("host").iter().count() != 1
        || headers.get_all("origin").iter().count() != 1
        || headers.get("host")?.to_str().ok()? != api.origin.split_once("://")?.1
        || headers.get("origin")?.to_str().ok()? != api.origin
        || ["authorization", "cookie", "x-csrf-token"]
            .iter()
            .any(|name| headers.get_all(*name).iter().count() > 1)
    {
        return None;
    }
    let authorization = headers.get("authorization").and_then(|v| v.to_str().ok());
    let cookie = headers
        .get("cookie")
        .and_then(|v| v.to_str().ok())
        .and_then(crate::web_api::session_cookie);
    if authorization.is_some() && cookie.is_some() {
        return None;
    }
    if let Some(auth) = authorization {
        api.tokens.authenticate(auth)
    } else {
        api.tokens.session(cookie?, None, false)
    }
}
fn failure(status: StatusCode) -> Response<Full<Bytes>> {
    Response::builder()
        .status(status)
        .body(Full::new(Bytes::new()))
        .unwrap()
}
pub(crate) fn upgrade(
    api: Arc<Api>,
    mut request: Request<Incoming>,
    mut stop: tokio::sync::watch::Receiver<bool>,
    permits: Arc<tokio::sync::Semaphore>,
) -> Response<Full<Bytes>> {
    if request.method() != hyper::Method::GET
        || request.uri().query().is_some()
        || current_grant(&api, request.headers()).is_none()
    {
        return failure(StatusCode::UNAUTHORIZED);
    }
    if ["sec-websocket-key", "sec-websocket-version", "upgrade"]
        .iter()
        .any(|name| request.headers().get_all(*name).iter().count() != 1)
    {
        return failure(StatusCode::BAD_REQUEST);
    }
    let Ok(permit) = permits.try_acquire_owned() else {
        return failure(StatusCode::SERVICE_UNAVAILABLE);
    };
    let headers = request.headers().clone();
    let mut handshake = Request::builder()
        .method(request.method())
        .uri(request.uri())
        .version(request.version())
        .body(())
        .unwrap();
    *handshake.headers_mut() = headers.clone();
    let Ok(response) =
        tokio_tungstenite::tungstenite::handshake::server::create_response(&handshake)
    else {
        return failure(StatusCode::BAD_REQUEST);
    };
    let upgraded = hyper::upgrade::on(&mut request);
    tokio::spawn(async move {
        let _permit = permit;
        let Ok(Ok(stream)) = tokio::time::timeout(Duration::from_secs(3), upgraded).await else {
            return;
        };
        let config = WebSocketConfig::default()
            .max_message_size(Some(8192))
            .max_frame_size(Some(8192));
        let mut socket = WebSocketStream::from_raw_socket(
            hyper_util::rt::TokioIo::new(stream),
            Role::Server,
            Some(config),
        )
        .await;
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Resume {
            cursor: Option<Cursor>,
        }
        let Ok(Some(Ok(Message::Text(initial)))) =
            tokio::time::timeout(Duration::from_secs(5), socket.next()).await
        else {
            return;
        };
        let Ok(resume) = serde_json::from_str::<Resume>(&initial) else {
            return;
        };
        let mut cursor = resume.cursor;
        let mut scenes = serde_json::Value::Null;
        let mut keypads=serde_json::Value::Null;
        let mut availability=serde_json::Value::Null;
        let mut directory=serde_json::Value::Null;
        let mut light_projection_state=serde_json::Value::Null;
    let mut motor_estimates=serde_json::Value::Null;
        let mut interval = tokio::time::interval(Duration::from_millis(500));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut unavailable = false;
        let mut previous_grant = None;
        let mut last_send = std::time::Instant::now();
        loop {
            tokio::select! {
                changed=stop.changed()=>{if changed.is_err() || *stop.borrow(){break;}},
                message=socket.next()=>match message {Some(Ok(Message::Ping(_)|Message::Pong(_)))=>{},_=>break},
                _=interval.tick()=>{
                    // Resolve current grants, expiry and revocation every tick;
                    // never preserve the handshake's authorization snapshot.
                    let Some(grant)=current_grant(&api,&headers) else {break;};
                    if previous_grant.as_ref().is_some_and(|previous|previous!=&grant) {cursor=None;}
                    previous_grant=Some(grant.clone());
                    let runtime=api.runtime.read().ok().and_then(|value|value.clone());
                    let Some(runtime)=runtime else {
                        if !unavailable {
                            if !matches!(tokio::time::timeout(Duration::from_secs(2),socket.send(Message::Text(json!({"type":"unavailable","selection_state":crate::scene_switches::SelectionState::Unavailable(crate::scene_switches::UnavailableReason::RuntimeUnavailable)}).to_string().into()))).await,Ok(Ok(()))){break;}
                            unavailable=true;cursor=None;
                        }
                        continue;
                    };
                    let ids=grant.devices.iter().map(homesense_core::DeviceId::new).collect::<Result<Vec<_>,_>>();
                    let Ok(ids)=ids else{break;};
                    let Ok(Reply::DeviceEvents(update))=runtime.control.request(Action::DeviceEvents{controller:grant.controller,revision:runtime.revision,ids,cursor:cursor.clone(),scope:format!("{:?}",grant.role)}).await else{
                        if !unavailable {
                            if !matches!(tokio::time::timeout(Duration::from_secs(2),socket.send(Message::Text(json!({"type":"unavailable","selection_state":crate::scene_switches::SelectionState::Unavailable(crate::scene_switches::UnavailableReason::ReadFailed)}).to_string().into()))).await,Ok(Ok(()))){break;}
                            unavailable=true;cursor=None;
                        }
                        continue;
                    };
                    let mut request=Request::builder().uri("/api/v1/scenes").body(Bytes::new()).unwrap();*request.headers_mut()=headers.clone();
                    let scene_response=api.handle(request).await;
                    if scene_response.status()!=StatusCode::OK {
                        if !unavailable {
                            let body=scene_response.into_body().collect().await.unwrap();
                            let detail=serde_json::from_slice::<serde_json::Value>(&body.to_bytes()).ok().and_then(|body|body.get("selection_state").cloned()).unwrap_or_else(||json!(crate::scene_switches::SelectionState::Unavailable(crate::scene_switches::UnavailableReason::ReadFailed)));
                            if !matches!(tokio::time::timeout(Duration::from_secs(2),socket.send(Message::Text(json!({"type":"unavailable","selection_state":detail}).to_string().into()))).await,Ok(Ok(()))){break;}
                            unavailable=true;cursor=None;
                        }
                        continue;
                    }
                    let body=scene_response.into_body().collect().await.unwrap();
                    let Ok(next_scenes)=serde_json::from_slice::<serde_json::Value>(&body.to_bytes()) else{break;};
                    // Publication may have replaced the runtime while the two
                    // reads awaited. Discard this mixed candidate and resync.
                    if api.runtime.read().ok().is_none_or(|current|current.as_ref().is_none_or(|current|current.revision!=runtime.revision || current.boot!=runtime.boot)){cursor=None;continue;}
                    if current_grant(&api,&headers).as_ref()!=Some(&grant) {cursor=None;continue;}
                    let next_light_projection=light_projection(&update.snapshot);
                    let next_motor_estimates=json!(update.snapshot.devices.iter().filter(|d|d.motor_estimate.is_some()).map(|d|(d.id.as_str(),&d.motor_estimate)).collect::<Vec<_>>());
                    let next_keypads=json!(update.snapshot.keypads);
                    let next_directory=json!(update.snapshot.directory(api.can_manage_configuration(&grant)));
                    let next_availability=json!(update.snapshot.devices.iter().map(|view|(view.id.as_str(),&view.availability)).collect::<Vec<_>>());
                    if cursor.as_ref()!=Some(&update.cursor) || next_scenes!=scenes || next_motor_estimates!=motor_estimates || next_keypads!=keypads || next_availability!=availability || next_directory!=directory || next_light_projection!=light_projection_state || unavailable {
                        let devices=crate::management_keypads::devices_json(&update.snapshot,&grant);
                        let frame=json!({"type":"state","cursor":update.cursor,"reset":update.reset,"devices":devices,"scenes":next_scenes["scenes"],"revision":next_scenes["revision"],"rooms":next_directory["rooms"],"floors":next_directory["floors"]}).to_string();
                        if frame.len()>512*1024 || !matches!(tokio::time::timeout(Duration::from_secs(2),socket.send(Message::Text(frame.into()))).await,Ok(Ok(()))){break;}
                        cursor=Some(update.cursor);scenes=next_scenes;motor_estimates=next_motor_estimates;keypads=next_keypads;availability=next_availability;directory=next_directory;light_projection_state=next_light_projection;unavailable=false;last_send=std::time::Instant::now();
                    }
                    if last_send.elapsed()>=Duration::from_secs(15) {
                        if !matches!(tokio::time::timeout(Duration::from_secs(2),socket.send(Message::Ping(Bytes::new()))).await,Ok(Ok(()))){break;}
                        last_send=std::time::Instant::now();
                    }
                }
            }
        }
        let _ = tokio::time::timeout(Duration::from_secs(1), socket.close(None)).await;
    });
    response.map(|_| Full::new(Bytes::new()))
}
