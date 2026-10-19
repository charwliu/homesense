//! Scoped management API. Commands use durable request fences and the running
//! automation owner; neither the HTTP layer nor its UI touches a gateway.
/// Bounded complete management catalog; write-batch and HAP subset limits remain separate.
pub const MAX_MANAGED_SCENES:usize=256;
pub(crate) fn check_catalog_capacity(count:usize)->Result<(),String>{if count>MAX_MANAGED_SCENES{Err("management scene capacity exceeded".into())}else{Ok(())}}
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
#[derive(Debug, Clone, Copy, Default, serde::Serialize)]
#[serde(rename_all="snake_case")]
pub enum HapStatus {
    #[default]
    Unknown,
    Disabled,
    Listening,
    Degraded,
    Reconfiguring,
}
pub struct Api {
    pub daily_backup: crate::daily_backup::Monitor,
    pub hap_bridges: std::sync::RwLock<BTreeMap<String,crate::hap_bridge_group::Status>>,
    pub hap_status: std::sync::RwLock<HapStatus>,
    pub backup_source: Option<std::path::PathBuf>,
    pub publisher: Option<crate::automation_publication::Publisher>,
    pub origin: String,
    pub tokens: Arc<Tokens>,
    pub runtime: std::sync::RwLock<Option<Runtime>>,
    pub receipts: Mutex<Receipts>,
    pub drafts: Option<(
        homesense_core::ExecutionContext,
        Mutex<crate::web_drafts::Drafts>,
    )>,
}
/// Coherent management view installed by the trusted runtime supervisor.
#[derive(Clone)]
pub struct Runtime {
    pub control: Control,
    pub revision: u64,
    pub boot: String,
    pub scenes: BTreeMap<String, String>,
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
pub(crate) fn receipt_response(code:StatusCode,value:serde_json::Value,expired:bool)->Response<Full<Bytes>> {let mut r=response(code,value);if expired{r.headers_mut().insert("x-homesense-receipt-state",hyper::header::HeaderValue::from_static("expired_preserved"));}r}
fn default_scene_on() -> bool { true }
fn selection_unavailable(reason: crate::scene_switches::UnavailableReason) -> Response<Full<Bytes>> {
    response(StatusCode::SERVICE_UNAVAILABLE, json!({
        "error":"scene_selection_unavailable",
        "selection_state":crate::scene_switches::SelectionState::Unavailable(reason),
    }))
}
async fn selection_details(
    control: &Control,
    controller: uuid::Uuid,
    revision: u64,
) -> Result<BTreeMap<String, crate::scene_switches::Details>, crate::scene_switches::UnavailableReason> {
    use crate::scene_switches::UnavailableReason;
    // A read-only request may complete late, but can never dispatch an action.
    let read_started=std::time::Instant::now();
    match tokio::time::timeout(std::time::Duration::from_millis(500), async {
        // SceneSelectionDetails performs its own revision, source/generation and
        // mapping-ready checks in the runtime. A separate diagnostics request
        // added a second drive quantum inside the same 500ms read budget.
        match control.request(Action::SceneSelectionDetails { controller, revision }).await {
            Ok(Reply::SceneSelectionDetails(states)) => Ok(states),
            Err(error) if error == "scene selection runtime not ready" => Err(UnavailableReason::RuntimeNotReady),
            _ => Err(UnavailableReason::ReadFailed),
        }
    }).await {
        Ok(result) => result,
        Err(_) => {
            log::warn!("scene_selection_read_timed_out deadline_ms=500 elapsed_ms={}",read_started.elapsed().as_millis());
            Err(UnavailableReason::ReadTimedOut)
        },
    }
}
impl Api {
    pub(crate) fn can_manage_configuration(&self,grant:&crate::web_auth::Grant)->bool {
        grant.role==crate::web_auth::Role::Administrator && self.drafts.is_some()
    }
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
        // The native App is the sole management UI. Retired entry points stay
        // unavailable even with an old asset version query or bearer token.
        if matches!(request.uri().path(), "/" | "/app.js" | "/draft-json.js" | "/style.css") {
            return deny(StatusCode::GONE, "native_app_required");
        }
        if request.uri().query().is_some()
            && !matches!(request.uri().path(), "/api/v1/config/versions" | "/api/v1/scene-history")
            && !(request.method() == hyper::Method::GET && request.uri().path() == "/api/v1/modules")
            && !(request.uri().path().starts_with("/api/v1/runs/") && request.uri().path().ends_with("/details"))
        {
            return deny(StatusCode::BAD_REQUEST, "query_not_supported");
        }
        // Minimal liveness reveals no runtime configuration or credentials.
        if request.uri().path() == "/health/live" {
            if request.method()!=hyper::Method::GET {return deny(StatusCode::METHOD_NOT_ALLOWED,"method_not_allowed");}
            if !request.body().is_empty() {return deny(StatusCode::BAD_REQUEST,"unexpected_body");}
            return response(StatusCode::OK,json!({"live":true}));
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
        match crate::web_management_receipts::Operation::route(request.method(),request.uri().path()) {
            Ok(Some(op))=>return crate::web_management_receipts::execute(self,&request,grant,op).await,
            Err(error)=>return deny(StatusCode::BAD_REQUEST,&error),
            Ok(None)=>{},
        }
        self.handle_authorized(&request,grant).await
    }
    pub(crate) async fn handle_authorized(&self,request:&Request<Bytes>,grant:crate::web_auth::Grant)->Response<Full<Bytes>> {
        let deny=|code,message|response(code,json!({"error":message}));
        let authorization=request.headers().get("authorization").and_then(|v|v.to_str().ok());
        let cookie=request.headers().get("cookie").and_then(|v|v.to_str().ok()).and_then(session_cookie);
        // Capture the write source before any async binding/query preflight.
        // A later lookup must not silently adopt a replacement generation.
        let control_source=if request.method()==hyper::Method::POST && matches!(request.uri().path(),"/api/v1/scene-actions"|"/api/v1/scene-cancellations"|"/api/v1/device-commands"|"/api/v1/module-commands"|"/api/v1/module-queries"|"/api/v1/scene-relative-actions"|"/api/v1/keypad-settings"|"/api/v1/keypad-actions") {
            let Some(source)=self.tokens.module_off_lease(authorization,cookie,request.headers().get("x-csrf-token").and_then(|v|v.to_str().ok())).filter(|source|source.matches_grant(&grant)) else{return deny(StatusCode::FORBIDDEN,"source_credential_required");};
            Some(source)
        } else {None};
        if request.method() == hyper::Method::GET && request.uri().path() == "/api/v1/session" {
            return response(
                StatusCode::OK,
                json!({"can_view_diagnostics":grant.role==crate::web_auth::Role::Administrator,"can_manage_configuration":self.can_manage_configuration(&grant),
                    "capabilities":{"keypad_actions":{"version":1,"path":"/api/v1/keypad-actions","gestures":["activate"],"indication_schema":1},"scene_placement":{"version":1,"scopes":["room","household","unassigned"]},"scene_lights":{"version":1,"state_source":"device_events"}}}),
            );
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
        if let Some(id)=request.uri().path().strip_prefix("/api/v1/credentials/").and_then(|path|path.strip_suffix("/revoke")) {
            if grant.role!=crate::web_auth::Role::Administrator {return deny(StatusCode::FORBIDDEN,"administrator_required");}
            if request.method()!=hyper::Method::POST {return deny(StatusCode::METHOD_NOT_ALLOWED,"method_not_allowed");}
            if request.headers().get("origin").and_then(|v|v.to_str().ok())!=Some(&self.origin) {return deny(StatusCode::FORBIDDEN,"origin_required");}
            if !request.body().is_empty() {return deny(StatusCode::BAD_REQUEST,"unexpected_body");}
            let id=match uuid::Uuid::parse_str(id) {Ok(id)=>id,Err(_)=>return deny(StatusCode::BAD_REQUEST,"invalid_credential_id")};
            return match self.tokens.revoke_managed(id,authorization,cookie,request.headers().get("x-csrf-token").and_then(|v|v.to_str().ok())) {
                Ok(())=>response(StatusCode::OK,json!({"revoked":id})),
                Err(code)=>deny(match code {"last_administrator"=>StatusCode::CONFLICT,"credential_not_found"=>StatusCode::NOT_FOUND,"administrator_required"=>StatusCode::FORBIDDEN,_=>StatusCode::SERVICE_UNAVAILABLE},code),
            };
        }
        if let Some(id)=request.uri().path().strip_prefix("/api/v1/credentials/") {
            if grant.role!=crate::web_auth::Role::Administrator {return deny(StatusCode::FORBIDDEN,"administrator_required");}
            if request.method()!=hyper::Method::PATCH {return deny(StatusCode::METHOD_NOT_ALLOWED,"method_not_allowed");}
            if request.headers().get("origin").and_then(|v|v.to_str().ok())!=Some(&self.origin) {return deny(StatusCode::FORBIDDEN,"origin_required");}
            let id=match uuid::Uuid::parse_str(id) {Ok(id)=>id,Err(_)=>return deny(StatusCode::BAD_REQUEST,"invalid_credential_id")};
            if request.body().len()>65536 {return deny(StatusCode::PAYLOAD_TOO_LARGE,"credential_request_too_large");}
            let input=match serde_json::from_slice::<crate::web_auth::CredentialUpdate>(request.body()) {Ok(input)=>input,Err(_)=>return deny(StatusCode::BAD_REQUEST,"invalid_credential_request")};
            return match self.tokens.update_grant(id,input,authorization,cookie,request.headers().get("x-csrf-token").and_then(|v|v.to_str().ok())) {
                Ok(updated)=>response(StatusCode::OK,json!(updated)),
                Err(code)=>deny(match code {"administrator_required"|"scope_expansion_forbidden"|"controller_mismatch"=>StatusCode::FORBIDDEN,"last_administrator"|"credential_changed"=>StatusCode::CONFLICT,"credential_not_found"=>StatusCode::NOT_FOUND,"invalid_credential_scope"=>StatusCode::BAD_REQUEST,_=>StatusCode::SERVICE_UNAVAILABLE},code),
            };
        }
        if request.uri().path()=="/api/v1/credentials" {
            if grant.role!=crate::web_auth::Role::Administrator {return deny(StatusCode::FORBIDDEN,"administrator_required");}
            if request.method()==hyper::Method::POST {
                if request.headers().get("origin").and_then(|v|v.to_str().ok())!=Some(&self.origin) {return deny(StatusCode::FORBIDDEN,"origin_required");}
                if request.body().len()>65536 {return deny(StatusCode::PAYLOAD_TOO_LARGE,"credential_request_too_large");}
                let input=match serde_json::from_slice::<crate::web_auth::CredentialRequest>(request.body()) {Ok(input)=>input,Err(_)=>return deny(StatusCode::BAD_REQUEST,"invalid_credential_request")};
                return match self.tokens.issue(input,authorization,cookie,request.headers().get("x-csrf-token").and_then(|v|v.to_str().ok())) {
                    Ok(issued)=>response(StatusCode::CREATED,json!(issued)),
                    Err(code)=>deny(match code {"administrator_required"|"scope_expansion_forbidden"=>StatusCode::FORBIDDEN,"invalid_credential_scope"=>StatusCode::BAD_REQUEST,"credential_capacity_reached"=>StatusCode::CONFLICT,_=>StatusCode::SERVICE_UNAVAILABLE},code),
                };
            }
            if request.method()!=hyper::Method::GET {return deny(StatusCode::METHOD_NOT_ALLOWED,"method_not_allowed");}
            if !request.body().is_empty() {return deny(StatusCode::BAD_REQUEST,"unexpected_body");}
            return match self.tokens.summaries() {
                Ok(credentials)=>response(StatusCode::OK,json!({"credentials":credentials})),
                Err(_)=>deny(StatusCode::SERVICE_UNAVAILABLE,"credential_store_unavailable"),
            };
        }
        if request.uri().path() == "/health/ready" {
            if grant.role!=crate::web_auth::Role::Administrator {return deny(StatusCode::FORBIDDEN,"administrator_required");}
            if request.method()!=hyper::Method::GET {return deny(StatusCode::METHOD_NOT_ALLOWED,"method_not_allowed");}
            if !request.body().is_empty() {return deny(StatusCode::BAD_REQUEST,"unexpected_body");}
            let runtime=self.runtime.read().ok().and_then(|state|state.clone());
            let ready=if let Some(runtime)=runtime {
                matches!(runtime.control.request(Action::Diagnostics{controller:grant.controller,revision:runtime.revision}).await,Ok(Reply::Diagnostics(s)) if s.ready())
            } else {false};
            return response(if ready {StatusCode::OK} else {StatusCode::SERVICE_UNAVAILABLE},json!({"ready":ready}));
        }
        // Take one coherent snapshot; never hold this lock across an await.
        // Admission rejects old generations if publication follows this read.
        let runtime = self.runtime.read().ok().and_then(|state| state.clone());
        let Some(runtime) = runtime else {
            if request.method() == hyper::Method::GET && request.uri().path() == "/api/v1/scenes" {
                return selection_unavailable(crate::scene_switches::UnavailableReason::RuntimeUnavailable);
            }
            return deny(StatusCode::SERVICE_UNAVAILABLE, "runtime_reconfiguring");
        };
        if request.uri().path()=="/api/v1/modules" {
            if grant.role!=crate::web_auth::Role::Administrator {return deny(StatusCode::FORBIDDEN,"administrator_required");}
            if request.method()!=hyper::Method::GET || !request.body().is_empty(){return deny(StatusCode::BAD_REQUEST,"invalid_module_query");}
            let mut offset=0usize;let mut limit=32usize;let mut revision=None;let mut seen=std::collections::BTreeSet::new();
            for part in request.uri().query().unwrap_or("").split('&').filter(|s|!s.is_empty()) {
                let Some((key,value))=part.split_once('=') else{return deny(StatusCode::BAD_REQUEST,"invalid_module_query");};
                if !seen.insert(key) {return deny(StatusCode::BAD_REQUEST,"invalid_module_query");}
                let Ok(number)=value.parse::<usize>() else{return deny(StatusCode::BAD_REQUEST,"invalid_module_query");};
                if number.to_string()!=value {return deny(StatusCode::BAD_REQUEST,"invalid_module_query");}
                match key {"offset"=>offset=number,"limit"=>limit=number,"revision"=>revision=Some(value),_=>return deny(StatusCode::BAD_REQUEST,"invalid_module_query")}
            }
            if offset>16320 || !(1..=64).contains(&limit){return deny(StatusCode::BAD_REQUEST,"invalid_module_query");}
            if (offset>0 && revision.is_none()) || revision.is_some_and(|r|r!=runtime.revision.to_string()){return deny(StatusCode::CONFLICT,"module_revision_changed");}
            return match runtime.control.request(Action::Modules{controller:grant.controller,revision:runtime.revision,offset,limit}).await {
                Ok(Reply::Modules(mut page))=>{
                    for module in &mut page.modules {mask_module_actions(&mut module.slots,&grant);}
                    response(StatusCode::OK,serde_json::to_value(page).unwrap())
                },
                _=>deny(StatusCode::SERVICE_UNAVAILABLE,"module_inventory_unavailable"),
            };
        }
        if request.uri().path() == "/api/v1/backups/inspect" {
            if grant.role != crate::web_auth::Role::Administrator {return deny(StatusCode::FORBIDDEN,"administrator_required");}
            if request.method()!=hyper::Method::POST {return deny(StatusCode::METHOD_NOT_ALLOWED,"method_not_allowed");}
            if request.headers().get("origin").and_then(|v|v.to_str().ok())!=Some(&self.origin) {return deny(StatusCode::FORBIDDEN,"origin_required");}
            if request.body().len()>1024 {return deny(StatusCode::PAYLOAD_TOO_LARGE,"selection_too_large");}
            let selection=match serde_json::from_slice::<crate::web_backups::Selection>(request.body()) {Ok(s) if s.validate().is_ok()=>s,_=>return deny(StatusCode::BAD_REQUEST,"invalid_backup_selection")};
            if !matches!(runtime.control.request(Action::Diagnostics{controller:grant.controller,revision:runtime.revision}).await,Ok(Reply::Diagnostics(_))) {return deny(StatusCode::SERVICE_UNAVAILABLE,"runtime_unavailable");}
            let Some(source)=self.backup_source.clone() else {return deny(StatusCode::SERVICE_UNAVAILABLE,"backups_unavailable");};
            let Some((context,_))=&self.drafts else {return deny(StatusCode::SERVICE_UNAVAILABLE,"backup_context_unavailable");};let context=*context;
            return match tokio::task::spawn_blocking(move||crate::web_backups::inspect(&source,context,selection)).await {
                Ok(Ok(value))=>response(StatusCode::OK,value),
                _=>deny(StatusCode::SERVICE_UNAVAILABLE,"backup_inspection_failed"),
            };
        }
        if request.uri().path() == "/api/v1/backups" {
            if grant.role != crate::web_auth::Role::Administrator {return deny(StatusCode::FORBIDDEN,"administrator_required");}
            if request.method()!=hyper::Method::GET {return deny(StatusCode::METHOD_NOT_ALLOWED,"method_not_allowed");}
            if !request.body().is_empty() {return deny(StatusCode::BAD_REQUEST,"unexpected_body");}
            if !matches!(runtime.control.request(Action::Diagnostics{controller:grant.controller,revision:runtime.revision}).await,Ok(Reply::Diagnostics(_))) {return deny(StatusCode::SERVICE_UNAVAILABLE,"runtime_unavailable");}
            let Some(source)=self.backup_source.clone() else {return deny(StatusCode::SERVICE_UNAVAILABLE,"backups_unavailable");};
            return match tokio::task::spawn_blocking(move||crate::web_backups::catalog(&source)).await {
                Ok(Ok(value))=>response(StatusCode::OK,value),
                _=>deny(StatusCode::SERVICE_UNAVAILABLE,"backup_catalog_unavailable"),
            };
        }
        if request.uri().path()=="/api/v1/away-off" {
            return crate::web_away_off::execute(self,&runtime,&grant,request).await;
        }
        if request.uri().path() == "/api/v1/diagnostics" {
            if grant.role != crate::web_auth::Role::Administrator {return deny(StatusCode::FORBIDDEN,"administrator_required");}
            if request.method()!=hyper::Method::GET {return deny(StatusCode::METHOD_NOT_ALLOWED,"method_not_allowed");}
            if !request.body().is_empty() {return deny(StatusCode::BAD_REQUEST,"unexpected_body");}
            return match runtime.control.request(Action::Diagnostics{controller:grant.controller,revision:runtime.revision}).await {
                Ok(Reply::Diagnostics(s))=>response(StatusCode::OK,json!({"boot":s.boot,"revision":s.epochs.configuration.to_string(),"transport_epoch":s.epochs.transport.to_string(),"control_epoch":s.epochs.control.to_string(),"queued":s.queued,"in_flight":s.in_flight,"failed":s.failed,"stopping":s.stopping,"publishing":s.publishing,"gateway_ready":s.gateway_ready,"controller_enabled":s.controller_enabled,"ready":s.ready(),"storage_healthy":s.storage_healthy,"valid_configuration_loaded":s.valid_configuration_loaded,"wire_capacity":s.wire_capacity,"native_events":s.native_events,"web_receipt_policy":self.receipts.lock().ok().map(|r|r.policy().clone()),"daily_backup":self.daily_backup.report(),"hap_bridges":self.hap_bridges.read().map(|s|s.clone()).unwrap_or_default(),"hap":self.hap_status.read().map(|state|*state).unwrap_or_default()})),
                _=>deny(StatusCode::SERVICE_UNAVAILABLE,"diagnostics_unavailable"),
            };
        }
        if request.uri().path() == "/api/v1/diagnostics/calendar" {
            if grant.role != crate::web_auth::Role::Administrator { return deny(StatusCode::FORBIDDEN,"administrator_required"); }
            if request.method() != hyper::Method::GET { return deny(StatusCode::METHOD_NOT_ALLOWED,"method_not_allowed"); }
            if !request.body().is_empty() { return deny(StatusCode::BAD_REQUEST,"unexpected_body"); }
            return match runtime.control.request(Action::ManagedCalendarStatus { controller: grant.controller, revision: runtime.revision }).await {
                Ok(Reply::CalendarStatus { boot, revision, status }) => response(StatusCode::OK,json!({"boot":boot,"revision":revision.to_string(),"calendar":status.map(|s|json!({
                    "schedule_count":s.clock.schedule_count,"wall_unix_ms":s.clock.wall_unix_ms.to_string(),"monotonic_ms":s.clock.monotonic_ms.to_string(),
                    "watermark_unix_ms":s.clock.watermark_unix_ms.to_string(),"stopped":s.clock.stopped,"last_clock_jump":s.clock.last_clock_jump,
                    "persisted_watermark_unix_ms":s.persisted_watermark_unix_ms.map(|v|v.to_string()),"startup_attempted":s.startup_attempted
                }))})),
                _ => deny(StatusCode::SERVICE_UNAVAILABLE,"calendar_diagnostics_unavailable"),
            };
        }
        if request.uri().path().starts_with("/api/v1/config/versions/") && request.uri().path().ends_with("/restore-draft") {
            let (code,body)=crate::web_history::restore(self,&runtime,&grant,request).await;
            return response(code,body);
        }
        if request.uri().path()=="/api/v1/config/versions" || request.uri().path().starts_with("/api/v1/config/versions/") {
            let (code,body)=crate::web_history::read(&runtime,&grant,request).await;
            return response(code,body);
        }
        if let Some(id) = request
            .uri()
            .path()
            .strip_prefix("/api/v1/config/activations/")
        {
            let (code, body, expired) =
                crate::web_activation::activate_scoped(self, &runtime, &grant, id, request).await;
            return receipt_response(code, body, expired);
        }
        if let Some(id) = request
            .uri()
            .path()
            .strip_prefix("/api/v1/config/previews/")
        {
            if grant.role != crate::web_auth::Role::Administrator {
                return deny(StatusCode::FORBIDDEN, "administrator_required");
            }
            if request.method() != hyper::Method::GET {
                return deny(StatusCode::METHOD_NOT_ALLOWED, "method_not_allowed");
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
            let draft = match drafts
                .lock()
                .map_err(|_| ())
                .and_then(|d| d.get(grant.controller, id).map_err(|_| ()))
            {
                Ok(Some(draft)) => draft,
                Ok(None) => return deny(StatusCode::NOT_FOUND, "draft_not_found"),
                Err(_) => {
                    return deny(StatusCode::SERVICE_UNAVAILABLE, "draft_storage_unavailable");
                }
            };
            let candidate = serde_json::to_vec(&draft.devices)
                .ok()
                .and_then(|bytes| homesense_config::Configuration::parse(&bytes, *context).ok());
            let Some(candidate) = candidate else {
                return deny(StatusCode::CONFLICT, "invalid_stored_draft");
            };
            return match runtime
                .control
                .request(Action::ConfigurationPreview {
                    controller: grant.controller,
                    revision: draft.expected_revision,
                    candidate,
                })
                .await
            {
                Ok(Reply::ConfigurationPreview(preview)) => response(
                    StatusCode::OK,
                    json!({"draft":id,"impact":preview,"reservation":false}),
                ),
                _ => deny(StatusCode::CONFLICT, "configuration_changed_or_unavailable"),
            };
        }
        if request.uri().path()=="/api/v1/config/drafts" {
            if grant.role!=crate::web_auth::Role::Administrator {return deny(StatusCode::FORBIDDEN,"administrator_required");}
            if request.method()!=hyper::Method::GET {return deny(StatusCode::METHOD_NOT_ALLOWED,"method_not_allowed");}
            let Some((_,drafts))=&self.drafts else {return deny(StatusCode::SERVICE_UNAVAILABLE,"configuration_management_unavailable");};
            return match drafts.lock().map_err(|_|()).and_then(|d|d.list(grant.controller).map_err(|_|())) {
                Ok(drafts)=>response(StatusCode::OK,json!({"drafts":drafts.iter().map(|d|json!({"id":d.id,"expected_revision":d.expected_revision.to_string(),"revision":d.revision.to_string()})).collect::<Vec<_>>()})),
                Err(_)=>deny(StatusCode::SERVICE_UNAVAILABLE,"draft_storage_unavailable"),
            };
        }
        if let Some(id) = request.uri().path().strip_prefix("/api/v1/config/drafts/") {
            if grant.role != crate::web_auth::Role::Administrator {
                return deny(StatusCode::FORBIDDEN, "administrator_required");
            }
            let module_calibration=id.ends_with("/module-calibration");
            let id=id.strip_suffix("/module-calibration").unwrap_or(id);
            let module_migration=id.ends_with("/module-migration");
            let id=id.strip_suffix("/module-migration").unwrap_or(id);
            let module_binding=id.ends_with("/module-binding");
            let id=id.strip_suffix("/module-binding").unwrap_or(id);
            let module_replacement=id.ends_with("/module-replacement");
            let id=id.strip_suffix("/module-replacement").unwrap_or(id);
            let calibration=id.ends_with("/motor-calibration");
            let id=id.strip_suffix("/motor-calibration").unwrap_or(id);
            let binding=id.ends_with("/device-binding");
            let id=id.strip_suffix("/device-binding").unwrap_or(id);
            let room_edit=id.ends_with("/room");
            let id=id.strip_suffix("/room").unwrap_or(id);
            let metadata=id.ends_with("/device-metadata");
            let id=id.strip_suffix("/device-metadata").unwrap_or(id);
            let archive=id.ends_with("/archive");
            let id=id.strip_suffix("/archive").unwrap_or(id);
            let source=id.ends_with("/source");
            let id=id.strip_suffix("/source").unwrap_or(id);
            if source && request.method()!=hyper::Method::GET {return deny(StatusCode::METHOD_NOT_ALLOWED,"method_not_allowed");}
            let Ok(id) = uuid::Uuid::parse_str(id) else {
                return deny(StatusCode::BAD_REQUEST, "invalid_draft_id");
            };
            let Some((context, drafts)) = &self.drafts else {
                return deny(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "configuration_management_unavailable",
                );
            };
            if module_binding || module_migration || module_calibration {
                if request.method()!=hyper::Method::POST{return deny(StatusCode::METHOD_NOT_ALLOWED,"method_not_allowed");}
                if request.headers().get("origin").and_then(|v|v.to_str().ok())!=Some(&self.origin){return deny(StatusCode::FORBIDDEN,"origin_required");}
                if request.headers().get("content-type").and_then(|v|v.to_str().ok())!=Some("application/json") || request.body().len()>8192{return deny(StatusCode::BAD_REQUEST,"invalid_module_edit");}
                if module_calibration {
                    let edit=match serde_json::from_slice::<crate::web_drafts::ModuleCalibrationEdit>(request.body()){Ok(e)=>e,Err(_)=>return deny(StatusCode::BAD_REQUEST,"invalid_module_edit")};
                    if edit.expected_revision!=runtime.revision.to_string(){return deny(StatusCode::CONFLICT,"stale_revision");}
                    return match drafts.lock(){Ok(mut drafts)=>match drafts.edit_module_calibration(grant.controller,id,edit,*context){Ok(id)=>response(StatusCode::OK,json!({"draft":id,"active":false})),Err(_)=>deny(StatusCode::BAD_REQUEST,"invalid_module_calibration")},Err(_)=>deny(StatusCode::SERVICE_UNAVAILABLE,"draft_storage_unavailable")};
                }
                if module_migration {
                    let edit=match serde_json::from_slice::<crate::module_migration::Edit>(request.body()){Ok(e)=>e,Err(_)=>return deny(StatusCode::BAD_REQUEST,"invalid_module_edit")};
                    if edit.expected_revision!=runtime.revision.to_string(){return deny(StatusCode::CONFLICT,"stale_revision");}
                    return match drafts.lock(){Ok(mut drafts)=>match drafts.migrate_modules(grant.controller,id,edit,*context){Ok(impact)=>response(StatusCode::OK,json!({"draft":impact.draft,"active":false,"impact":impact})),Err(_)=>deny(StatusCode::BAD_REQUEST,"invalid_module_migration")},Err(_)=>deny(StatusCode::SERVICE_UNAVAILABLE,"draft_storage_unavailable")};
                }
                let edit=match serde_json::from_slice::<crate::web_drafts::ModuleBindingEdit>(request.body()){Ok(e)=>e,Err(_)=>return deny(StatusCode::BAD_REQUEST,"invalid_module_edit")};
                if edit.expected_revision!=runtime.revision.to_string(){return deny(StatusCode::CONFLICT,"stale_revision");}
                return match drafts.lock(){Ok(mut drafts)=>match drafts.edit_module_binding(grant.controller,id,edit,*context){Ok(id)=>response(StatusCode::OK,json!({"draft":id,"active":false})),Err(_)=>deny(StatusCode::BAD_REQUEST,"invalid_module_binding")},Err(_)=>deny(StatusCode::SERVICE_UNAVAILABLE,"draft_storage_unavailable")};
            }
            if module_replacement {
                if request.method()!=hyper::Method::POST{return deny(StatusCode::METHOD_NOT_ALLOWED,"method_not_allowed");}
                if request.headers().get("origin").and_then(|v|v.to_str().ok())!=Some(&self.origin){return deny(StatusCode::FORBIDDEN,"origin_required");}
                if request.headers().get("content-type").and_then(|v|v.to_str().ok())!=Some("application/json") || request.body().len()>8192{return deny(StatusCode::BAD_REQUEST,"invalid_module_replacement");}
                let edit=match serde_json::from_slice::<crate::module_replace::Edit>(request.body()){Ok(e)=>e,Err(_)=>return deny(StatusCode::BAD_REQUEST,"invalid_module_replacement")};
                if edit.expected_revision!=runtime.revision.to_string(){return deny(StatusCode::CONFLICT,"stale_revision");}
                return match drafts.lock(){
                    Ok(mut drafts)=>match drafts.replace_module(grant.controller,id,edit,*context){Ok(impact)=>response(StatusCode::OK,json!({"draft":impact.draft,"active":false,"impact":impact})),Err(_)=>deny(StatusCode::BAD_REQUEST,"invalid_module_replacement")},
                    Err(_)=>deny(StatusCode::SERVICE_UNAVAILABLE,"draft_storage_unavailable"),
                };
            }
            if room_edit {
                if request.method()!=hyper::Method::POST {return deny(StatusCode::METHOD_NOT_ALLOWED,"method_not_allowed");}
                if request.headers().get("origin").and_then(|v|v.to_str().ok())!=Some(&self.origin) {return deny(StatusCode::FORBIDDEN,"origin_required");}
                if request.body().len()>2048 {return deny(StatusCode::PAYLOAD_TOO_LARGE,"edit_too_large");}
                let edit=match serde_json::from_slice::<crate::web_drafts::RoomEdit>(request.body()){Ok(edit)=>edit,Err(_)=>return deny(StatusCode::BAD_REQUEST,"invalid_room_edit")};
                if edit.expected_revision!=runtime.revision.to_string(){return deny(StatusCode::CONFLICT,"stale_revision");}
                return match drafts.lock() {
                    Ok(mut drafts)=>match drafts.edit_room(grant.controller,id,edit,*context){Ok(id)=>response(StatusCode::OK,json!({"draft":id,"active":false})),Err(e) if e=="room_in_use"=>deny(StatusCode::BAD_REQUEST,"room_in_use"),Err(_)=>deny(StatusCode::BAD_REQUEST,"invalid_room_edit")},
                    Err(_)=>deny(StatusCode::SERVICE_UNAVAILABLE,"draft_storage_unavailable"),
                };
            }
            if calibration {
                if request.method()!=hyper::Method::POST {return deny(StatusCode::METHOD_NOT_ALLOWED,"method_not_allowed");}
                if request.headers().get("origin").and_then(|v|v.to_str().ok())!=Some(&self.origin) {return deny(StatusCode::FORBIDDEN,"origin_required");}
                if request.body().len()>8192 {return deny(StatusCode::PAYLOAD_TOO_LARGE,"edit_too_large");}
                let edit=match serde_json::from_slice::<crate::web_drafts::MotorCalibrationEdit>(request.body()){Ok(edit)=>edit,Err(_)=>return deny(StatusCode::BAD_REQUEST,"invalid_motor_calibration")};
                if edit.expected_revision!=runtime.revision.to_string(){return deny(StatusCode::CONFLICT,"stale_revision");}
                return match drafts.lock() {
                    Ok(mut drafts)=>match drafts.edit_motor_calibration(grant.controller,id,edit,*context){Ok(id)=>response(StatusCode::OK,json!({"draft":id,"active":false})),Err(_)=>deny(StatusCode::BAD_REQUEST,"invalid_motor_calibration")},
                    Err(_)=>deny(StatusCode::SERVICE_UNAVAILABLE,"draft_storage_unavailable"),
                };
            }
            if binding {
                if request.method()!=hyper::Method::POST {return deny(StatusCode::METHOD_NOT_ALLOWED,"method_not_allowed");}
                if request.headers().get("origin").and_then(|v|v.to_str().ok())!=Some(&self.origin) {return deny(StatusCode::FORBIDDEN,"origin_required");}
                if request.body().len()>2048 {return deny(StatusCode::PAYLOAD_TOO_LARGE,"edit_too_large");}
                let edit=match serde_json::from_slice::<crate::web_drafts::DeviceBindingEdit>(request.body()){Ok(edit)=>edit,Err(_)=>return deny(StatusCode::BAD_REQUEST,"invalid_device_binding")};
                if edit.expected_revision!=runtime.revision.to_string(){return deny(StatusCode::CONFLICT,"stale_revision");}
                return match drafts.lock() {
                    Ok(mut drafts)=>match drafts.edit_binding(grant.controller,id,edit,*context){Ok(id)=>response(StatusCode::OK,json!({"draft":id,"active":false})),Err(_)=>deny(StatusCode::BAD_REQUEST,"invalid_device_binding")},
                    Err(_)=>deny(StatusCode::SERVICE_UNAVAILABLE,"draft_storage_unavailable"),
                };
            }
            if metadata {
                if request.method()!=hyper::Method::POST {return deny(StatusCode::METHOD_NOT_ALLOWED,"method_not_allowed");}
                if request.headers().get("origin").and_then(|v|v.to_str().ok())!=Some(&self.origin) {return deny(StatusCode::FORBIDDEN,"origin_required");}
                if request.body().len()>2048 {return deny(StatusCode::PAYLOAD_TOO_LARGE,"edit_too_large");}
                let edit=match serde_json::from_slice::<crate::web_drafts::DeviceMetadataEdit>(request.body()){Ok(edit)=>edit,Err(_)=>return deny(StatusCode::BAD_REQUEST,"invalid_device_edit")};
                if edit.expected_revision!=runtime.revision.to_string(){return deny(StatusCode::CONFLICT,"stale_revision");}
                return match drafts.lock() {
                    Ok(mut drafts)=>match drafts.edit_device(grant.controller,id,edit,*context){Ok(id)=>response(StatusCode::OK,json!({"draft":id,"active":false})),Err(_)=>deny(StatusCode::BAD_REQUEST,"invalid_device_edit")},
                    Err(_)=>deny(StatusCode::SERVICE_UNAVAILABLE,"draft_storage_unavailable"),
                };
            }
            if archive {
                if request.method()!=hyper::Method::POST {return deny(StatusCode::METHOD_NOT_ALLOWED,"method_not_allowed");}
                if request.headers().get("origin").and_then(|v|v.to_str().ok())!=Some(&self.origin) {return deny(StatusCode::FORBIDDEN,"origin_required");}
                if !request.body().is_empty() {return deny(StatusCode::BAD_REQUEST,"unexpected_body");}
                return match drafts.lock().map_err(|_|()).and_then(|d|d.archive(grant.controller,id).map_err(|_|())) {
                    Ok(true)=>response(StatusCode::OK,json!({"archived":true,"id":id})),
                    Ok(false)=>deny(StatusCode::NOT_FOUND,"draft_not_found"),
                    Err(_)=>deny(StatusCode::SERVICE_UNAVAILABLE,"draft_storage_unavailable"),
                };
            }
            if request.method() == hyper::Method::GET {
                return match drafts
                    .lock()
                    .map_err(|_| ())
                    .and_then(|d| d.get(grant.controller, id).map_err(|_| ()))
                {
                    Ok(Some(draft)) => {
                        if source {
                            response(StatusCode::OK,json!({"id":draft.id,"expected_revision":draft.expected_revision.to_string(),"revision":draft.revision.to_string(),"devices":draft.devices.to_string(),"automation":draft.automation.to_string()}))
                        } else {response(StatusCode::OK, serde_json::to_value(draft).unwrap())}
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
            if input.expected_revision != runtime.revision {
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
        if request.uri().path()=="/api/v1/scene-history" {
            let (code,body)=crate::web_scene_history::read(&runtime,&grant,request).await;
            return response(code,body);
        }
        if request.uri().path().starts_with("/api/v1/runs/") && request.uri().path().ends_with("/details") {
            let (code,body)=crate::web_run_details::read(&runtime,&grant,request).await;
            return response(code,body);
        }
        if let Some(key)=request.uri().path().strip_prefix("/api/v1/runs/") {
            if request.method()!=hyper::Method::GET {return deny(StatusCode::METHOD_NOT_ALLOWED,"method_not_allowed");}
            if !request.body().is_empty(){return deny(StatusCode::BAD_REQUEST,"unexpected_body");}
            let Some((boot,run))=key.split_once('/') else {return deny(StatusCode::BAD_REQUEST,"invalid_run");};
            if uuid::Uuid::parse_str(boot).is_err() || run.is_empty() || run.len()>128 || !run.bytes().all(|c|c.is_ascii_alphanumeric()||c==b'-'||c==b'_') {return deny(StatusCode::BAD_REQUEST,"invalid_run");}
            return match runtime.control.request(Action::RunSummary{controller:grant.controller,revision:runtime.revision,boot:boot.into(),run:run.into()}).await {
                Ok(Reply::RunSummary(Some((scene,v)))) if runtime.scenes.contains_key(&scene) && grant.may_read(&scene)=>response(StatusCode::OK,json!({"scene":scene,"boot":v.boot,"run":v.run,"revision":v.revision.to_string(),"started_ms":v.started_ms.to_string(),"updated_ms":v.updated_ms.to_string(),"version":v.version.to_string(),"status":format!("{:?}",v.status)})),
                Ok(Reply::RunSummary(_))=>deny(StatusCode::NOT_FOUND,"run_not_found"),
                _=>deny(StatusCode::SERVICE_UNAVAILABLE,"run_summary_unavailable"),
            };
        }
        if request.method() == hyper::Method::GET && request.uri().path() == "/api/v1/scenes" {
            let selection = match selection_details(&runtime.control, grant.controller, runtime.revision).await {
                Ok(states) => states,
                Err(reason) => return selection_unavailable(reason),
            };
            let ids=runtime.scenes.keys().filter(|id|grant.may_read(id)).cloned().collect();
            let Ok(Reply::SceneCatalog(mut catalog))=runtime.control.request(Action::SceneCatalog{controller:grant.controller,revision:runtime.revision,scenes:ids}).await else {return deny(StatusCode::SERVICE_UNAVAILABLE,"scene_catalog_unavailable");};
            let mut items = vec![];
            for (id, name) in &runtime.scenes {
                if !grant.may_read(id) {
                    continue;
                }
                let Some(entry)=catalog.remove(id) else{return deny(StatusCode::SERVICE_UNAVAILABLE,"scene_catalog_incomplete");};
                let execution_kind=entry.execution_kind;
                let action_gate=entry.action_gate;
                let tracking=entry.tracking;
                let activity=format!("{:?}",entry.activity);
                let name=tracking.as_ref().and_then(|v|v.display_name.as_ref()).unwrap_or(name);
                let last=entry.last_run.map(|v|json!({"boot":v.boot,"run":v.run,"status":format!("{:?}",v.status)})).unwrap_or(serde_json::Value::Null);
                let light_count=entry.lights.len();
                let lights:Vec<_>=entry.lights.into_iter().filter(|member|grant.may_read_device(&member.id)).collect();
                let lights_complete=lights.len()==light_count;
                items.push(json!({"id":id,"name":name,"room":entry.room,"scope":entry.scope,"lights":lights,"lights_complete":lights_complete,"action_gate":action_gate,"execution_kind":execution_kind,"history_available":execution_kind==homesense_config::rule_document::SceneExecutionKind::Timed,"activity":activity,"activity_source":if let Some(tracking)=&tracking{tracking.activity_source}else if execution_kind==homesense_config::rule_document::SceneExecutionKind::Immediate{"not_configured"}else{"final_light_targets"},"tracking":tracking,"last_run":last,"can_control":grant.may_control(id) && tracking.as_ref().is_none_or(|v|v.action_available) && !selection.get(id).is_some_and(|s|s.pending_relative),"activity_kind":if tracking.is_some(){"read_only"}else if execution_kind==homesense_config::rule_document::SceneExecutionKind::Immediate{"unconfigured"}else{"load_feedback"},"switch_kind":if selection.contains_key(id){"selection"}else{"momentary"},"selected":selection.get(id).and_then(|s|s.selected),"selection_state":selection.get(id).map(|s|s.selection_state),"selection_source":selection.get(id).map(|s|s.selection_source),"historical_selected":selection.get(id).and_then(|s|s.historical_selected),"pending_relative":selection.get(id).is_some_and(|s|s.pending_relative),"relative_unknown_allowed":selection.get(id).is_some_and(|s|s.relative_unknown_allowed),"observation_current":selection.get(id).and_then(|s|s.observation_current),"observation_boot":selection.get(id).and_then(|s|s.observation_boot.as_ref()),"observed_ms":selection.get(id).and_then(|s|s.observed_ms.as_ref())}));
            }
            return response(StatusCode::OK, json!({"revision":runtime.revision.to_string(),"scenes":items}));
        }
        if request.method() == hyper::Method::GET && matches!(request.uri().path(), "/api/v1/devices" | "/api/v1/rooms") {
            let ids = grant
                .devices
                .iter()
                .map(|id| homesense_core::DeviceId::new(id.clone()))
                .collect::<Result<Vec<_>, _>>();
            let Ok(ids) = ids else {
                return deny(StatusCode::SERVICE_UNAVAILABLE, "invalid_device_scope");
            };
            let Ok(Reply::Devices(snapshot)) = runtime
                .control
                .request(Action::Devices {
                    controller: grant.controller,
                    revision: runtime.revision,
                    ids,
                })
                .await
            else {
                return deny(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "device_snapshot_unavailable",
                );
            };
            let directory=snapshot.directory(self.can_manage_configuration(&grant));
            if request.uri().path()=="/api/v1/rooms" {
                return response(StatusCode::OK,json!({"boot":snapshot.boot,"revision":snapshot.revision.to_string(),"rooms":directory.rooms,"floors":directory.floors}));
            }
            let devices = crate::management_keypads::devices_json(&snapshot, &grant);
            return response(
                StatusCode::OK,
                json!({"boot":snapshot.boot,"revision":snapshot.revision,"revision_text":snapshot.revision.to_string(),"sequence":snapshot.sequence,"devices":devices,"rooms":directory.rooms,"floors":directory.floors}),
            );
        }
        if request.method()==hyper::Method::GET && let Some(key)=request.uri().path().strip_prefix("/api/v1/scene-relative-requests/") {
            if key.is_empty() || key.len()>128 || !key.bytes().all(|b|b.is_ascii_alphanumeric()||b==b'-'||b==b'_') {return deny(StatusCode::BAD_REQUEST,"invalid_request_id");}
            let result=runtime.control.request(Action::SceneRelativeStatus{controller:grant.controller,revision:runtime.revision,request_id:key.to_owned()}).await;
            return match result {
                Ok(Reply::SceneRelative(receipt)) if grant.may_read(&receipt.scene) && runtime.scenes.contains_key(&receipt.scene)=>response(StatusCode::OK,json!({"request_id":receipt.key,"scene":receipt.scene,"boot":receipt.boot,"status":receipt.status,"selected":receipt.selected,"retry":false})),
                _=>deny(StatusCode::NOT_FOUND,"relative_request_not_found"),
            };
        }
        if let Some(key)=request.uri().path().strip_prefix("/api/v1/module-queries/") {
            if grant.role!=crate::web_auth::Role::Administrator {return deny(StatusCode::FORBIDDEN,"administrator_required");}
            if request.method()!=hyper::Method::GET || !request.body().is_empty() || request.uri().query().is_some() || key.is_empty() || key.len()>128 || !key.bytes().all(|b|b.is_ascii_alphanumeric()||b==b'-'||b==b'_') {return deny(StatusCode::BAD_REQUEST,"invalid_query_receipt");}
            let owned=self.receipts.lock().ok().and_then(|r|r.lookup_scoped(&grant.controller.to_string(),&crate::web_receipts::Endpoint::ModuleQueries,key).ok().flatten().map(|r|(r.fingerprint,r.response,r.expired)));
            let Some((fingerprint,Some(receipt),receipt_expired))=owned else {return deny(StatusCode::NOT_FOUND,"owned_query_not_found");};
            let decoded=serde_json::from_str::<serde_json::Value>(&fingerprint).ok().zip(serde_json::from_str::<serde_json::Value>(&receipt).ok());
            let Some((fingerprint,receipt))=decoded else{return deny(StatusCode::CONFLICT,"query_receipt_unknown");};
            if fingerprint["action"]!="module_query" || fingerprint["revision"]!=runtime.revision || receipt["boot"]!=runtime.boot || receipt["outcome"]!="accepted" {return deny(StatusCode::CONFLICT,"query_not_in_current_runtime");}
            let Some(module_id)=fingerprint["module_id"].as_str() else{return deny(StatusCode::CONFLICT,"query_receipt_unknown");};
            let scope=match serde_json::from_value::<crate::module_queries::Scope>(fingerprint["scope"].clone()){Ok(v)=>v,Err(_)=>return deny(StatusCode::CONFLICT,"query_receipt_unknown")};
            let commands=receipt["commands"].as_array().and_then(|ids|ids.iter().map(|id|id.as_str()?.parse::<u64>().ok()).collect::<Option<Vec<_>>>());
            let Some(commands)=commands else{return deny(StatusCode::CONFLICT,"query_receipt_unknown");};
            return match runtime.control.request(Action::ModuleQueryStatus{controller:grant.controller,revision:runtime.revision,module_id:module_id.to_owned(),scope,commands}).await {
                Ok(Reply::ModuleQueryStatus(mut status))=>{
                    mask_module_actions(&mut status.channels,&grant);
                    mask_module_actions(&mut status.module.slots,&grant);
                    receipt_response(StatusCode::OK,json!({"request_id":key,"boot":runtime.boot,"revision":runtime.revision.to_string(),"status":status}),receipt_expired)
                },
                _=>deny(StatusCode::CONFLICT,"query_scope_unavailable"),
            };
        }
        if request.method() != hyper::Method::POST
            || !matches!(
                request.uri().path(),
                "/api/v1/scene-actions" | "/api/v1/scene-cancellations" | "/api/v1/device-commands" | "/api/v1/module-commands" | "/api/v1/module-queries" | "/api/v1/scene-relative-actions" | "/api/v1/keypad-settings" | "/api/v1/keypad-actions"
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
        if request.uri().path()=="/api/v1/keypad-actions" {
            return self.keypad_action(request,&runtime,&grant,control_source).await;
        }
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Start {
            scene: String,
            request_id: String,
            #[serde(default = "default_scene_on")]
            on: bool,
        }
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Cancel {
            start_request_id: String,
            request_id: String,
        }
        let principal = grant.controller.to_string();
        let mut source_receipt_expired=false;
        let is_device = matches!(request.uri().path(),"/api/v1/device-commands"|"/api/v1/module-commands"|"/api/v1/module-queries"|"/api/v1/keypad-settings");
        let (scene, request_id, action, fingerprint) = if request.uri().path()=="/api/v1/keypad-settings" {
            if grant.role!=crate::web_auth::Role::Administrator {return deny(StatusCode::FORBIDDEN,"administrator_required");}
            #[derive(serde::Deserialize)]
            #[serde(deny_unknown_fields)]
            struct SettingRequest{keypad:String,revision:String,request_id:String,command:crate::management_keypads::Setting}
            let body=match serde_json::from_slice::<SettingRequest>(request.body()){Ok(b)=>b,Err(_)=>return deny(StatusCode::BAD_REQUEST,"invalid_body")};
            if body.revision!=runtime.revision.to_string(){return deny(StatusCode::CONFLICT,"stale_revision");}
            let Some(lease)=control_source.clone().filter(|l|l.controller()==grant.controller && l.valid()) else{return deny(StatusCode::FORBIDDEN,"panel_credential_required");};
            let checked=runtime.control.request(Action::KeypadSetting{controller:grant.controller,revision:runtime.revision,keypad:body.keypad.clone(),lease:lease.clone(),setting:body.command.clone(),submit:false}).await;
            if !matches!(checked,Ok(Reply::CommandsAccepted(_))) {return deny(StatusCode::FORBIDDEN,"keypad_settings_not_permitted_or_incompatible");}
            let fingerprint=json!({"revision":runtime.revision,"action":"keypad_setting","keypad":body.keypad,"command":body.command}).to_string();
            (String::new(),body.request_id,Action::KeypadSetting{controller:grant.controller,revision:runtime.revision,keypad:body.keypad,lease,setting:body.command,submit:true},fingerprint)
        } else if request.uri().path()=="/api/v1/module-queries" {
            if grant.role!=crate::web_auth::Role::Administrator{return deny(StatusCode::FORBIDDEN,"administrator_required");}
            #[derive(serde::Deserialize)]
            #[serde(deny_unknown_fields)]
            struct ModuleQuery{module_id:String,scope:crate::module_queries::Scope,revision:String,request_id:String}
            let body=match serde_json::from_slice::<ModuleQuery>(request.body()){Ok(b)=>b,Err(_)=>return deny(StatusCode::BAD_REQUEST,"invalid_body")};
            if body.revision!=runtime.revision.to_string(){return deny(StatusCode::CONFLICT,"stale_revision");}
            if body.module_id.is_empty() || body.module_id.len()>128 {return deny(StatusCode::BAD_REQUEST,"invalid_body");}
            if body.request_id.is_empty() || body.request_id.len()>128 || !body.request_id.bytes().all(|b|b.is_ascii_alphanumeric()||b==b'-'||b==b'_') {return deny(StatusCode::BAD_REQUEST,"invalid_request_id");}
            let checked=runtime.control.request(Action::ModuleQuery{controller:grant.controller,revision:runtime.revision,module_id:body.module_id.clone(),scope:body.scope.clone(),submit:false}).await;
            if !matches!(checked,Ok(Reply::ModuleQuery(_))) {return deny(StatusCode::BAD_REQUEST,"module_query_unsupported_or_unavailable");}
            let fingerprint=json!({"revision":runtime.revision,"action":"module_query","module_id":body.module_id,"scope":body.scope}).to_string();
            (String::new(),body.request_id,Action::ModuleQuery{controller:grant.controller,revision:runtime.revision,module_id:body.module_id,scope:body.scope,submit:true},fingerprint)
        } else if request.uri().path()=="/api/v1/module-commands" {
            if grant.role!=crate::web_auth::Role::Administrator{return deny(StatusCode::FORBIDDEN,"administrator_required");}
            #[derive(serde::Deserialize)]
            #[serde(deny_unknown_fields)]
            struct ModuleCommand{module_id:String,channel:u8,revision:String,request_id:String,command:crate::management_devices::DeviceAction}
            let body=match serde_json::from_slice::<ModuleCommand>(request.body()){Ok(body)=>body,Err(_)=>return deny(StatusCode::BAD_REQUEST,"invalid_body")};
            if matches!(body.command,crate::management_devices::DeviceAction::MotorPress {..}) {return deny(StatusCode::BAD_REQUEST,"module_motor_press_unsupported");}
            if body.revision!=runtime.revision.to_string(){return deny(StatusCode::CONFLICT,"stale_revision");}
            if body.channel==0 || body.module_id.is_empty() || body.module_id.len()>128{return deny(StatusCode::BAD_REQUEST,"invalid_body");}
            let device=match runtime.control.request(Action::ModuleBinding{controller:grant.controller,revision:runtime.revision,module_id:body.module_id.clone(),channel:body.channel}).await{Ok(Reply::ModuleBinding(id))=>id,_=>return deny(StatusCode::BAD_REQUEST,"module_channel_unavailable")};
            if !grant.may_control_device(device.as_str()){return deny(StatusCode::FORBIDDEN,"device_not_permitted");}
            let fingerprint=json!({"revision":runtime.revision,"action":"module_command","module_id":body.module_id,"channel":body.channel,"command":body.command}).to_string();
            (device.as_str().to_owned(),body.request_id,Action::ModuleCommand{controller:grant.controller,revision:runtime.revision,module_id:body.module_id,channel:body.channel,allowed_devices:[device.as_str().to_owned()].into(),action:body.command},fingerprint)
        } else if is_device {
            #[derive(serde::Deserialize)]
            #[serde(deny_unknown_fields)]
            struct DeviceCommand {
                device: String,
                #[serde(default)]
                revision: Option<String>,
                request_id: String,
                command: crate::management_devices::DeviceAction,
            }
            let Ok(body) = serde_json::from_slice::<DeviceCommand>(request.body()) else {
                return deny(StatusCode::BAD_REQUEST, "invalid_body");
            };
            if let crate::management_devices::DeviceAction::KeypadPress{keypad,key}=&body.command
                && (body.revision.is_none() || !(1..=6).contains(key) || homesense_core::DeviceId::new(keypad).is_err()) {
                return deny(StatusCode::BAD_REQUEST,"invalid_keypad_press");
            }
            if matches!(body.command,crate::management_devices::DeviceAction::MotorPress {..}) && body.revision.is_none() {return deny(StatusCode::BAD_REQUEST,"motor_press_revision_required");}
            if body.revision.is_some_and(|revision|revision!=runtime.revision.to_string()){return deny(StatusCode::CONFLICT,"stale_revision");}
            if !grant.may_control_device(&body.device) {
                return deny(StatusCode::FORBIDDEN, "device_not_permitted");
            }
            let Ok(device) = homesense_core::DeviceId::new(body.device.clone()) else {
                return deny(StatusCode::BAD_REQUEST, "invalid_device");
            };
            if matches!(body.command, crate::management_devices::DeviceAction::Brightness {level} if level>100)
            {
                return deny(StatusCode::BAD_REQUEST, "invalid_brightness");
            }
            if matches!(body.command,crate::management_devices::DeviceAction::MotorPress {..}) {
                match tokio::time::timeout(std::time::Duration::from_millis(500),runtime.control.request(Action::MotorBinding {controller:grant.controller,revision:runtime.revision,device:device.clone()})).await {
                    Ok(Ok(Reply::ModuleBinding(_)))=>{},
                    Ok(Err(_))=>return deny(StatusCode::BAD_REQUEST,"motor_press_target_unavailable"),
                    _=>return deny(StatusCode::SERVICE_UNAVAILABLE,"motor_binding_unavailable"),
                }
            }
            let fingerprint=json!({"action":"device","revision":runtime.revision,"device":body.device,"command":body.command}).to_string();
            let action=if matches!(body.command,crate::management_devices::DeviceAction::PanelLed{..}|crate::management_devices::DeviceAction::PanelLedOff) {
                let Some(lease)=control_source.clone().filter(|l|l.controller()==grant.controller && l.valid()) else{return deny(StatusCode::FORBIDDEN,"panel_credential_required");};
                Action::PanelDeviceCommand{controller:grant.controller,revision:runtime.revision,device,action:body.command,lease}
            } else {Action::DeviceCommand{controller:grant.controller,revision:runtime.revision,device,action:body.command}};
            (
                String::new(),
                body.request_id,
                action,
                fingerprint,
            )
        } else if request.uri().path()=="/api/v1/scene-relative-actions" {
            #[derive(serde::Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Relative {scene:String,request_id:String}
            let Ok(body)=serde_json::from_slice::<Relative>(request.body()) else {return deny(StatusCode::BAD_REQUEST,"invalid_body");};
            let fingerprint=json!({"revision":runtime.revision,"scene":body.scene,"action":"relative"}).to_string();
            (body.scene.clone(),body.request_id.clone(),Action::SceneRelative{controller:grant.controller,revision:runtime.revision,scene:body.scene,request_id:body.request_id},fingerprint)
        } else if request.uri().path() == "/api/v1/scene-actions" {
            let Ok(body) = serde_json::from_slice::<Start>(request.body()) else {
                return deny(StatusCode::BAD_REQUEST, "invalid_body");
            };
            let fingerprint =
                json!({"revision":runtime.revision,"scene":body.scene,"action":if body.on {"start"} else {"off"}})
                    .to_string();
            (
                body.scene.clone(),
                body.request_id,
                Action::SceneAction {
                    controller: grant.controller,
                    revision: runtime.revision,
                    scene: body.scene,
                    execute: body.on,
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
                .and_then(|r| r.lookup_scoped(&principal,&crate::web_receipts::Endpoint::SceneActions,&body.start_request_id).ok().flatten().map(|r|(r.fingerprint,r.response,r.expired)));
            let Some((fingerprint, Some(receipt), expired)) = record else {
                return deny(StatusCode::NOT_FOUND, "owned_start_not_found");
            };
            source_receipt_expired=expired;
            let parsed = serde_json::from_str::<serde_json::Value>(&fingerprint).ok();
            let receipt = serde_json::from_str::<serde_json::Value>(&receipt).ok();
            let Some((scene, run)) = parsed.as_ref().zip(receipt.as_ref()).and_then(|(p, r)| {
                if !matches!(p["action"].as_str(), Some("start" | "off"))
                    || p["revision"] != runtime.revision
                    || r["outcome"] != "accepted"
                    || r["boot"] != runtime.boot
                    || r["runtime_instance"] != runtime.control.instance_id().to_string()
                {
                    return None;
                }
                Some((p["scene"].as_str()?.to_owned(), r["run"].as_u64()?))
            }) else {
                return deny(StatusCode::CONFLICT, "start_not_in_current_runtime");
            };
            let fingerprint=json!({"revision":runtime.revision,"scene":scene,"action":"cancel","start_request_id":body.start_request_id}).to_string();
            (
                scene,
                body.request_id,
                Action::SceneCancel {
                    controller: grant.controller,
                    revision: runtime.revision,
                    run,
                },
                fingerprint,
            )
        };
        if !is_device && (!grant.may_control(&scene) || !runtime.scenes.contains_key(&scene)) {
            return deny(StatusCode::FORBIDDEN, "scene_not_permitted");
        }
        let command_source=if crate::source_auth::Source::supports(&action) {
            let Some(lease)=control_source else{return deny(StatusCode::FORBIDDEN,"source_credential_required");};
            let Some(source)=crate::source_auth::Source::capture(lease,&action,Some(&scene)) else{return deny(StatusCode::FORBIDDEN,"source_scope_changed");};
            Some(source)
        }else{None};
        let endpoint=match crate::web_receipts::Endpoint::control(request.uri().path()){Ok(e)=>e,Err(_)=>return deny(StatusCode::NOT_FOUND,"not_found")};
        let claim = self
            .receipts
            .lock()
            .map_err(|_| ())
            .and_then(|mut receipts| {
                receipts
                    .claim_scoped(&principal,&endpoint,&request_id,&fingerprint)
                    .map_err(|_| ())
            });
        let claim=match claim {Ok(c)=>c,Err(_)=>return deny(StatusCode::SERVICE_UNAVAILABLE,"persistence_unavailable")};
        let expiry_header=|mut r:Response<Full<Bytes>>|{if source_receipt_expired{r.headers_mut().insert("x-homesense-start-receipt-state",hyper::header::HeaderValue::from_static("expired_preserved"));}
            if claim.expired{r.headers_mut().insert("x-homesense-receipt-state",hyper::header::HeaderValue::from_static("expired_preserved"));}r};
        match claim.claim {
            Claim::Existing(Some(value)) => {
                return expiry_header(response(
                    StatusCode::OK,
                    serde_json::from_str(&value).unwrap_or(json!({"outcome":"unknown"})),
                ));
            }
            Claim::Existing(None) => {
                return expiry_header(response(
                    StatusCode::ACCEPTED,
                    json!({"request_id":request_id,"outcome":"unknown","retry":false}),
                ));
            }
            Claim::Conflict => return deny(StatusCode::CONFLICT, "idempotency_conflict"),
            Claim::New => {}
        }
        let relative=request.uri().path()=="/api/v1/scene-relative-actions";
        let controlled=if let Some(source)=command_source {runtime.control.request_authorized(action,source).await}else{runtime.control.request(action).await};
        let outcome = match controlled {
            Ok(Reply::SceneAccepted(Some(run))) => {
                json!({"request_id":request_id,"outcome":"accepted","boot":runtime.boot,"run":run,"runtime_instance":runtime.control.instance_id().to_string()})
            }
            Ok(Reply::ImmediateAccepted(receipt))=>json!({"request_id":request_id,"outcome":"accepted","execution_kind":"immediate","boot":runtime.boot,"receipt":receipt,"retry":false}),
            Ok(Reply::SceneRelative(receipt))=>json!({"request_id":request_id,"outcome":"accepted","status":receipt.status,"boot":receipt.boot,"selected":receipt.selected,"retry":false}),
            Err(error) if relative=>json!({"request_id":request_id,"outcome":"unknown","error":error,"retry":false}),
            Ok(Reply::SceneAccepted(None))=>json!({"request_id":request_id,"outcome":"unchanged","retry":false}),
            Ok(Reply::ModuleQuery(query))=>json!({"request_id":request_id,"outcome":"accepted","boot":query.boot,"revision":query.revision,"transport_epoch":query.transport_epoch,"commands":query.commands,"retry":false}),
            Ok(Reply::CommandsAccepted(commands)) => {
                json!({"request_id":request_id,"outcome":"accepted","boot":runtime.boot,"commands":commands})
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
                    .finish_scoped(&principal,&endpoint,&request_id,&outcome.to_string())
                    .map_err(|_| ())
            })
            .is_err()
        {
            return deny(
                StatusCode::SERVICE_UNAVAILABLE,
                "receipt_unavailable_outcome_unknown",
            );
        }
        expiry_header(response(StatusCode::ACCEPTED, outcome))
    }

    async fn keypad_action(&self,request:&Request<Bytes>,runtime:&Runtime,grant:&crate::web_auth::Grant,
        lease:Option<crate::web_auth::ModuleOffLease>)->Response<Full<Bytes>> {
        let Ok(input)=serde_json::from_slice::<crate::keypad_actions::Request>(request.body()) else {
            return response(StatusCode::BAD_REQUEST,json!({"outcome":"rejected","error":"invalid_body","retry":false}));
        };
        let rejected=|status,error|response(status,json!({"request_id":input.request_id,"outcome":"rejected","error":error,"retry":false,"indication":null}));
        if input.validate().is_err(){return rejected(StatusCode::BAD_REQUEST,"invalid_body");}
        if input.revision!=runtime.revision.to_string(){return rejected(StatusCode::CONFLICT,"stale_revision");}
        let action=Action::KeypadAction{controller:grant.controller,request:input.clone()};
        let Some(lease)=lease else {return rejected(StatusCode::FORBIDDEN,"source_credential_required");};
        let principal=format!("keypad:{}",lease.credential_id());
        let fingerprint=crate::keypad_actions::fingerprint(&input,&lease);
        let Some(source)=crate::source_auth::Source::capture(lease,&action,None) else {
            return rejected(StatusCode::FORBIDDEN,"source_credential_required");
        };
        let endpoint=crate::web_receipts::Endpoint::KeypadActions;
        let claim=self.receipts.lock().ok().and_then(|mut ledger|ledger.claim_scoped(&principal,&endpoint,&input.request_id,&fingerprint).ok());
        let Some(claim)=claim else{return rejected(StatusCode::SERVICE_UNAVAILABLE,"persistence_unavailable");};
        match claim.claim {
            Claim::Existing(Some(value))=>return receipt_response(StatusCode::OK,
                serde_json::from_str(&value).unwrap_or(json!({"request_id":input.request_id,"outcome":"unknown","retry":false})),claim.expired),
            Claim::Existing(None)=>return receipt_response(StatusCode::ACCEPTED,
                json!({"request_id":input.request_id,"outcome":"unknown","error":"execution_outcome_unknown","retry":false,"indication":null}),claim.expired),
            Claim::Conflict=>return rejected(StatusCode::CONFLICT,"idempotency_conflict"),
            Claim::New=>{},
        }
        let (status,outcome)=match runtime.control.request_authorized(action,source).await {
            Ok(Reply::KeypadAccepted(receipt))=>(StatusCode::ACCEPTED,json!(receipt)),
            Ok(Reply::KeypadRejected(error))=>(error.status(),json!({"request_id":input.request_id,"outcome":"rejected","error":error,"retry":false})),
            _=>(StatusCode::ACCEPTED,json!({"request_id":input.request_id,"outcome":"unknown","error":"execution_outcome_unknown","retry":false,"indication":null})),
        };
        if !self.receipts.lock().ok().is_some_and(|mut ledger|ledger.finish_scoped(&principal,&endpoint,&input.request_id,&outcome.to_string()).is_ok()) {
            return response(StatusCode::SERVICE_UNAVAILABLE,json!({"request_id":input.request_id,"outcome":"unknown","error":"receipt_unavailable_outcome_unknown","retry":false,"indication":null}));
        }
        receipt_response(status,outcome,claim.expired)
    }
}

pub(crate) fn session_cookie(header: &str) -> Option<&str> {
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

pub(crate) fn device_json(
    device: &crate::management_devices::DeviceView,
    grant: &crate::web_auth::Grant,
) -> serde_json::Value {
    let value_json = |value| match value {
        Some(homesense_core::Observation::Switch(on)) => json!({"on":on}),
        Some(homesense_core::Observation::Level(level)) => {
            json!({"level":level.get(),"on":level.get()>0})
        }
        _ => serde_json::Value::Null,
    };
    let observed=value_json(device.state.observed);
    let reported=value_json(device.reported);
    let light = matches!(
        device.state.kind,
        homesense_core::ModuleKind::Switch | homesense_core::ModuleKind::Dimmer
    );
    json!({"id":device.id.as_str(),"catalog_visible":device.catalog_visible,"communication_error":device.communication_error,"name":device.name,"room":device.room,"kind":format!("{:?}",device.state.kind),"observed":observed,"reported":reported,"state_authority":if device.recorded {"pending_target"} else {"feedback"},"quality":format!("{:?}",device.state.quality),"availability":device.availability,"action_gate":{"available":device.availability.as_ref().is_none_or(|view|view.state=="ready")},"observed_at_ms":device.state.observed_at_ms,"pending":device.state.pending_command,"version":device.state.version,"motor_calibrated":device.motor_calibrated,"motor_estimate":device.motor_estimate,"can_control":(light || device.motor_calibrated) && grant.may_control_device(device.id.as_str())})
}

fn mask_module_actions(slots: &mut [crate::management_modules::Slot], grant: &crate::web_auth::Grant) {
    for slot in slots {
        if let Some(binding) = &mut slot.binding && !grant.may_control_device(&binding.id) {
            binding.actions.clear();
        }
    }
}

#[cfg(test)]
mod selection_quantum_tests {
    use super::*;
    #[tokio::test]
    async fn selection_read_needs_one_runtime_quantum_and_keeps_the_original_read_deadline() {
        let directory=std::env::temp_dir().join(format!("scene-read-quantum-{}",uuid::Uuid::new_v4()));std::fs::create_dir(&directory).unwrap();
        let path=directory.join("runtime.sqlite");let rules=br#"{"schema_version":1,"configuration_revision":1,"variables":[],"rules":[]}"#;
        homesense_storage::Store::open(&path).unwrap().publish_automation(None,include_bytes!("../../../config/simulation.json"),rules,homesense_core::ExecutionContext::Simulation,&[]).unwrap();
        let controller=uuid::Uuid::new_v4();
        let admission=crate::admission::Admission::open(path,crate::admission::Options {context:homesense_core::ExecutionContext::Simulation,..Default::default()},move|id|id==controller).await.unwrap();
        admission.synchronized(1,1).await.unwrap();
        let bundle=admission.automation_bundle(controller,admission.task_generation().await.unwrap()).await.unwrap();
        let document=bundle.automation.unwrap();
        let feedback=crate::feedback::Feedback::new(&bundle.configuration,homesense_core::ExecutionContext::Simulation,uuid::Uuid::parse_str(admission.boot_id()).unwrap().as_u128(),1).unwrap();
        let (control,mut receiver)=crate::automation_control::channel(controller,admission.task_generation().await.unwrap());
        let mut scenes=crate::scene_scheduler::SceneScheduler::new(admission.clone(),controller,8,admission.monotonic_ms()).unwrap();
        let read=async {let result=selection_details(&control,controller,1).await;assert!(result.unwrap().is_empty());};
        // A single drive call must suffice. No wall-clock sleep or weakened
        // product timeout: the old second request has no subsequent quantum.
        let drive=async {receiver.poll(&admission,&document,&mut scenes,&feedback,5000,None).await;};
        tokio::join!(read,drive);
        // No runtime polling: genuine timeout must still be reported, not cached.
        assert!(matches!(selection_details(&control,controller,1).await,Err(crate::scene_switches::UnavailableReason::ReadTimedOut)));
        drop(receiver);admission.shutdown().await.unwrap();std::fs::remove_dir_all(directory).unwrap();
    }
}

#[cfg(test)]
#[path="selection_read_fairness_tests.rs"]
mod selection_read_fairness_tests;

#[cfg(test)]
#[path="selection_actor_queue_tests.rs"]
mod selection_actor_queue_tests;
