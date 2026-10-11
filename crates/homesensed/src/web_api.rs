//! Scoped management API. Commands use durable request fences and the running
//! automation owner; neither the HTTP layer nor its UI touches a gateway.
use crate::{
    automation_control::{Action, Control, Reply},
    web_auth::Tokens,
    web_receipts::{Claim, Receipts},
};
use http_body_util::Full;
use hyper::{Request, Response, StatusCode, body::Bytes};
use serde_json::json;
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};
pub struct Api {
    pub origin: String,
    pub tokens: Arc<Tokens>,
    pub control: Control,
    pub revision: u64,
    pub boot: String,
    pub scenes: BTreeMap<String, String>,
    pub receipts: Mutex<Receipts>,
    pub drafts: Option<(
        homesense_core::ExecutionContext,
        Mutex<crate::web_drafts::Drafts>,
    )>,
}
fn response(code: StatusCode, value: serde_json::Value) -> Response<Full<Bytes>> {
    Response::builder()
        .status(code)
        .header("content-type", "application/json")
        .header("cache-control", "no-store")
        .header("x-content-type-options", "nosniff")
        .body(Full::new(Bytes::from(value.to_string())))
        .unwrap()
}
impl Api {
    pub async fn handle(&self, request: Request<Bytes>) -> Response<Full<Bytes>> {
        let deny = |code, message| response(code, json!({"error":message}));
        let expected_host = self
            .origin
            .strip_prefix("http://")
            .or_else(|| self.origin.strip_prefix("https://"));
        if expected_host.is_none()
            || request.headers().get_all("host").iter().count() != 1
            || request.headers().get("host").and_then(|v| v.to_str().ok()) != expected_host
        {
            return deny(StatusCode::FORBIDDEN, "invalid_host");
        }
        if request.headers().get_all("origin").iter().count() > 1
            || request
                .headers()
                .get("origin")
                .is_some_and(|v| v.to_str().ok() != Some(&self.origin))
        {
            return deny(StatusCode::FORBIDDEN, "invalid_origin");
        }
        if request.uri().query().is_some() {
            return deny(StatusCode::BAD_REQUEST, "query_not_supported");
        }
        if request.method() == hyper::Method::GET && request.uri().path() == "/" {
            return Response::builder().header("content-type","text/html; charset=utf-8").header("cache-control","no-store")
                .header("content-security-policy","default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'none'")
                .body(Full::new(Bytes::from_static(include_bytes!("web/index.html")))).unwrap();
        }
        if request.method() == hyper::Method::GET && request.uri().path() == "/app.js" {
            return Response::builder()
                .header("content-type", "text/javascript")
                .header("x-content-type-options", "nosniff")
                .body(Full::new(Bytes::from_static(include_bytes!("web/app.js"))))
                .unwrap();
        }
        let authorization = request
            .headers()
            .get("authorization")
            .and_then(|v| v.to_str().ok());
        if request.headers().get_all("authorization").iter().count() > 1
            || request.headers().get_all("cookie").iter().count() > 1
            || request.headers().get_all("x-csrf-token").iter().count() > 1
        {
            return deny(StatusCode::UNAUTHORIZED, "ambiguous_credentials");
        }
        let cookie = request
            .headers()
            .get("cookie")
            .and_then(|v| v.to_str().ok())
            .and_then(session_cookie);
        if request.method() == hyper::Method::POST && request.uri().path() == "/api/v1/session" {
            if request
                .headers()
                .get("origin")
                .and_then(|v| v.to_str().ok())
                != Some(&self.origin)
            {
                return deny(StatusCode::FORBIDDEN, "origin_required");
            }
            let Some(session) = authorization.and_then(|h| self.tokens.login(h)) else {
                return deny(StatusCode::UNAUTHORIZED, "authentication_required");
            };
            let mut result = response(
                StatusCode::OK,
                json!({"csrf":session.csrf,"expires_in":900}),
            );
            let secure = if self.origin.starts_with("https://") {
                "; Secure"
            } else {
                ""
            };
            result.headers_mut().insert(
                "set-cookie",
                format!(
                    "homesense_session={}; HttpOnly; SameSite=Strict; Path=/; Max-Age=900{secure}",
                    session.cookie
                )
                .parse()
                .unwrap(),
            );
            return result;
        }
        if authorization.is_some() && cookie.is_some() {
            return deny(StatusCode::UNAUTHORIZED, "ambiguous_credentials");
        }
        let write = request.method() != hyper::Method::GET;
        let grant = if let Some(header) = authorization {
            self.tokens.authenticate(header)
        } else {
            cookie.and_then(|cookie| {
                self.tokens.session(
                    cookie,
                    request
                        .headers()
                        .get("x-csrf-token")
                        .and_then(|v| v.to_str().ok()),
                    write,
                )
            })
        };
        let Some(grant) = grant else {
            return deny(StatusCode::UNAUTHORIZED, "authentication_required");
        };
        if request.method() == hyper::Method::GET && request.uri().path() == "/api/v1/session" {
            return response(
                StatusCode::OK,
                json!({"can_manage_configuration":grant.role==crate::web_auth::Role::Administrator && self.drafts.is_some()}),
            );
        }
        if let Some(id) = request.uri().path().strip_prefix("/api/v1/config/drafts/") {
            if grant.role != crate::web_auth::Role::Administrator {
                return deny(StatusCode::FORBIDDEN, "administrator_required");
            }
            let Ok(id) = uuid::Uuid::parse_str(id) else {
                return deny(StatusCode::BAD_REQUEST, "invalid_draft_id");
            };
            let Some((context, drafts)) = &self.drafts else {
                return deny(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "configuration_management_unavailable",
                );
            };
            if request.method() == hyper::Method::GET {
                return match drafts
                    .lock()
                    .map_err(|_| ())
                    .and_then(|d| d.get(grant.controller, id).map_err(|_| ()))
                {
                    Ok(Some(draft)) => {
                        response(StatusCode::OK, serde_json::to_value(draft).unwrap())
                    }
                    Ok(None) => deny(StatusCode::NOT_FOUND, "draft_not_found"),
                    Err(_) => deny(StatusCode::SERVICE_UNAVAILABLE, "draft_storage_unavailable"),
                };
            }
            if request.method() != hyper::Method::POST {
                return deny(StatusCode::METHOD_NOT_ALLOWED, "method_not_allowed");
            }
            if request
                .headers()
                .get("origin")
                .and_then(|v| v.to_str().ok())
                != Some(&self.origin)
            {
                return deny(StatusCode::FORBIDDEN, "origin_required");
            }
            if request
                .headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok())
                != Some("application/json")
                || request.body().len() > crate::web_drafts::MAX_BYTES
            {
                return deny(StatusCode::BAD_REQUEST, "invalid_body");
            }
            #[derive(serde::Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Input {
                expected_revision: u64,
                revision: u64,
                devices: serde_json::Value,
                automation: serde_json::Value,
            }
            let Ok(input) = serde_json::from_slice::<Input>(request.body()) else {
                return deny(StatusCode::BAD_REQUEST, "invalid_body");
            };
            if input.expected_revision != self.revision {
                return deny(StatusCode::CONFLICT, "configuration_changed");
            }
            let draft = crate::web_drafts::Draft {
                id,
                expected_revision: input.expected_revision,
                revision: input.revision,
                devices: input.devices,
                automation: input.automation,
            };
            return match drafts.lock() {
                Ok(mut drafts) => match drafts.save(grant.controller, &draft, *context) {
                    Ok(()) => response(
                        StatusCode::OK,
                        json!({"draft":id,"revision":draft.revision,"active":false}),
                    ),
                    Err(_) => deny(
                        StatusCode::UNPROCESSABLE_ENTITY,
                        "draft_invalid_conflicting_or_unavailable",
                    ),
                },
                Err(_) => deny(StatusCode::SERVICE_UNAVAILABLE, "draft_storage_unavailable"),
            };
        }
        if request.method() == hyper::Method::DELETE && request.uri().path() == "/api/v1/session" {
            if request
                .headers()
                .get("origin")
                .and_then(|v| v.to_str().ok())
                != Some(&self.origin)
            {
                return deny(StatusCode::FORBIDDEN, "origin_required");
            }
            if let Some(cookie) = cookie {
                self.tokens.logout(cookie);
            }
            let mut result = response(StatusCode::OK, json!({"logged_out":true}));
            result.headers_mut().insert(
                "set-cookie",
                "homesense_session=; HttpOnly; SameSite=Strict; Path=/; Max-Age=0"
                    .parse()
                    .unwrap(),
            );
            return result;
        }
        if request.method() == hyper::Method::GET && request.uri().path() == "/api/v1/scenes" {
            let mut items = vec![];
            for (id, name) in &self.scenes {
                if !grant.may_read(id) {
                    continue;
                }
                let activity = match self
                    .control
                    .request(Action::SceneActivity(id.clone()))
                    .await
                {
                    Ok(Reply::Activity(Some(v))) => format!("{v:?}"),
                    _ => "Unknown".into(),
                };
                let last = match self.control.request(Action::LastRun(id.clone())).await {
                    Ok(Reply::LastRun(Some(v))) => {
                        json!({"boot":v.boot,"run":v.run,"status":format!("{:?}",v.status)})
                    }
                    _ => serde_json::Value::Null,
                };
                items.push(json!({"id":id,"name":name,"activity":activity,"last_run":last,"can_control":grant.may_control(id)}));
            }
            return response(StatusCode::OK, json!({"scenes":items}));
        }
        if request.method() == hyper::Method::GET && request.uri().path() == "/api/v1/devices" {
            let ids = grant
                .devices
                .iter()
                .map(|id| homesense_core::DeviceId::new(id.clone()))
                .collect::<Result<Vec<_>, _>>();
            let Ok(ids) = ids else {
                return deny(StatusCode::SERVICE_UNAVAILABLE, "invalid_device_scope");
            };
            let Ok(Reply::Devices(snapshot)) = self
                .control
                .request(Action::Devices {
                    controller: grant.controller,
                    revision: self.revision,
                    ids,
                })
                .await
            else {
                return deny(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "device_snapshot_unavailable",
                );
            };
            let devices = snapshot.devices.into_iter().map(|device| {
                let observed = match device.state.observed {
                    Some(homesense_core::Observation::Switch(on)) => json!({"on":on}),
                    Some(homesense_core::Observation::Level(level)) => json!({"level":level.get(),"on":level.get()>0}),
                    _ => serde_json::Value::Null,
                };
                let light = matches!(device.state.kind, homesense_core::ModuleKind::Switch | homesense_core::ModuleKind::Dimmer);
                json!({"id":device.id.as_str(),"name":device.name,"kind":format!("{:?}",device.state.kind),"observed":observed,"quality":format!("{:?}",device.state.quality),"observed_at_ms":device.state.observed_at_ms,"pending":device.state.pending_command,"version":device.state.version,"can_control":light && grant.may_control_device(device.id.as_str())})
            }).collect::<Vec<_>>();
            return response(
                StatusCode::OK,
                json!({"boot":snapshot.boot,"revision":snapshot.revision,"sequence":snapshot.sequence,"devices":devices}),
            );
        }
        if request.method() != hyper::Method::POST
            || !matches!(
                request.uri().path(),
                "/api/v1/scene-actions" | "/api/v1/scene-cancellations" | "/api/v1/device-commands"
            )
        {
            return deny(StatusCode::NOT_FOUND, "not_found");
        }
        // Browser writes require an exact origin; API clients must supply it too.
        if request
            .headers()
            .get("origin")
            .and_then(|v| v.to_str().ok())
            != Some(&self.origin)
        {
            return deny(StatusCode::FORBIDDEN, "origin_required");
        }
        if request
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            != Some("application/json")
            || request.body().len() > 4096
        {
            return deny(StatusCode::BAD_REQUEST, "invalid_body");
        }
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Start {
            scene: String,
            request_id: String,
        }
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Cancel {
            start_request_id: String,
            request_id: String,
        }
        let principal = grant.controller.to_string();
        let is_device = request.uri().path() == "/api/v1/device-commands";
        let (scene, request_id, action, fingerprint) = if is_device {
            #[derive(serde::Deserialize)]
            #[serde(deny_unknown_fields)]
            struct DeviceCommand {
                device: String,
                request_id: String,
                command: crate::management_devices::LightAction,
            }
            let Ok(body) = serde_json::from_slice::<DeviceCommand>(request.body()) else {
                return deny(StatusCode::BAD_REQUEST, "invalid_body");
            };
            if !grant.may_control_device(&body.device) {
                return deny(StatusCode::FORBIDDEN, "device_not_permitted");
            }
            let Ok(device) = homesense_core::DeviceId::new(body.device.clone()) else {
                return deny(StatusCode::BAD_REQUEST, "invalid_device");
            };
            if matches!(body.command, crate::management_devices::LightAction::Brightness {level} if level>100)
            {
                return deny(StatusCode::BAD_REQUEST, "invalid_brightness");
            }
            let fingerprint=json!({"action":"device","revision":self.revision,"device":body.device,"command":body.command}).to_string();
            (
                String::new(),
                body.request_id,
                Action::DeviceCommand {
                    controller: grant.controller,
                    revision: self.revision,
                    device,
                    action: body.command,
                },
                fingerprint,
            )
        } else if request.uri().path() == "/api/v1/scene-actions" {
            let Ok(body) = serde_json::from_slice::<Start>(request.body()) else {
                return deny(StatusCode::BAD_REQUEST, "invalid_body");
            };
            let fingerprint =
                json!({"revision":self.revision,"scene":body.scene,"action":"start"}).to_string();
            (
                body.scene.clone(),
                body.request_id,
                Action::SceneAction {
                    controller: grant.controller,
                    revision: self.revision,
                    scene: body.scene,
                    execute: true,
                },
                fingerprint,
            )
        } else {
            let Ok(body) = serde_json::from_slice::<Cancel>(request.body()) else {
                return deny(StatusCode::BAD_REQUEST, "invalid_body");
            };
            let record = self
                .receipts
                .lock()
                .ok()
                .and_then(|r| r.lookup(&principal, &body.start_request_id).ok().flatten());
            let Some((fingerprint, Some(receipt))) = record else {
                return deny(StatusCode::NOT_FOUND, "owned_start_not_found");
            };
            let parsed = serde_json::from_str::<serde_json::Value>(&fingerprint).ok();
            let receipt = serde_json::from_str::<serde_json::Value>(&receipt).ok();
            let Some((scene, run)) = parsed.as_ref().zip(receipt.as_ref()).and_then(|(p, r)| {
                if p["action"] != "start"
                    || p["revision"] != self.revision
                    || r["outcome"] != "accepted"
                    || r["boot"] != self.boot
                {
                    return None;
                }
                Some((p["scene"].as_str()?.to_owned(), r["run"].as_u64()?))
            }) else {
                return deny(StatusCode::CONFLICT, "start_not_in_current_runtime");
            };
            let fingerprint=json!({"revision":self.revision,"scene":scene,"action":"cancel","start_request_id":body.start_request_id}).to_string();
            (
                scene,
                body.request_id,
                Action::SceneCancel {
                    controller: grant.controller,
                    revision: self.revision,
                    run,
                },
                fingerprint,
            )
        };
        if !is_device && (!grant.may_control(&scene) || !self.scenes.contains_key(&scene)) {
            return deny(StatusCode::FORBIDDEN, "scene_not_permitted");
        }
        let claim = self
            .receipts
            .lock()
            .map_err(|_| ())
            .and_then(|mut receipts| {
                receipts
                    .claim(&principal, &request_id, &fingerprint)
                    .map_err(|_| ())
            });
        match claim {
            Ok(Claim::Existing(Some(value))) => {
                return response(
                    StatusCode::OK,
                    serde_json::from_str(&value).unwrap_or(json!({"outcome":"unknown"})),
                );
            }
            Ok(Claim::Existing(None)) => {
                return response(
                    StatusCode::ACCEPTED,
                    json!({"request_id":request_id,"outcome":"unknown","retry":false}),
                );
            }
            Ok(Claim::Conflict) => return deny(StatusCode::CONFLICT, "idempotency_conflict"),
            Err(_) => return deny(StatusCode::SERVICE_UNAVAILABLE, "persistence_unavailable"),
            Ok(Claim::New) => {}
        }
        let outcome = match self.control.request(action).await {
            Ok(Reply::SceneAccepted(Some(run))) => {
                json!({"request_id":request_id,"outcome":"accepted","boot":self.boot,"run":run})
            }
            Ok(Reply::CommandsAccepted(commands)) => {
                json!({"request_id":request_id,"outcome":"accepted","boot":self.boot,"commands":commands})
            }
            Ok(Reply::Cancelled(count)) => {
                json!({"request_id":request_id,"outcome":"cancelled","cancelled":count})
            }
            _ => json!({"request_id":request_id,"outcome":"unknown","retry":false}),
        };
        if self
            .receipts
            .lock()
            .map_err(|_| ())
            .and_then(|mut receipts| {
                receipts
                    .finish(&principal, &request_id, &outcome.to_string())
                    .map_err(|_| ())
            })
            .is_err()
        {
            return deny(
                StatusCode::SERVICE_UNAVAILABLE,
                "receipt_unavailable_outcome_unknown",
            );
        }
        response(StatusCode::ACCEPTED, outcome)
    }
}

fn session_cookie(header: &str) -> Option<&str> {
    let mut values = header
        .split(';')
        .filter_map(|part| part.trim().strip_prefix("homesense_session="));
    let value = values.next()?;
    if values.next().is_some() || value.len() != 64 {
        None
    } else {
        Some(value)
    }
}
