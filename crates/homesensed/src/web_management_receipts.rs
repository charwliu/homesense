//! Request fences for administration mutations; no credential secret is persisted.
use crate::{
    web_api::Api,
    web_auth::{Grant, Role},
    web_receipts::{Claim, Endpoint},
};
use http_body_util::{BodyExt, Full};
use hyper::{Request, Response, StatusCode, body::Bytes};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use uuid::Uuid;
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Operation {
    CredentialIssue,
    CredentialUpdate(Uuid),
    CredentialRevoke(Uuid),
    Draft(Uuid, DraftOperation),
    RestoreDraft(u64),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DraftOperation {
    Save,
    DeviceMetadata,
    DeviceBinding,
    MotorCalibration,
    ModuleBinding,
    ModuleCalibration,
    ModuleMigration,
    ModuleReplacement,
    Room,
    Archive,
}
impl Operation {
    pub fn route(method: &hyper::Method, path: &str) -> Result<Option<Self>, String> {
        if method == hyper::Method::GET {
            return Ok(None);
        }
        if path == "/api/v1/credentials" && method == hyper::Method::POST {
            return Ok(Some(Self::CredentialIssue));
        }
        if let Some(id) = path.strip_prefix("/api/v1/credentials/") {
            if method == hyper::Method::POST
                && let Some(id) = id.strip_suffix("/revoke")
            {
                return Uuid::parse_str(id)
                    .map(|id| Some(Self::CredentialRevoke(id)))
                    .map_err(|_| "invalid_credential_id".into());
            }
            if method == hyper::Method::PATCH {
                return Uuid::parse_str(id)
                    .map(|id| Some(Self::CredentialUpdate(id)))
                    .map_err(|_| "invalid_credential_id".into());
            }
            return Ok(None);
        }
        if method != hyper::Method::POST {
            return Ok(None);
        }
        if let Some(rest) = path.strip_prefix("/api/v1/config/drafts/") {
            let (id, op) = if let Some((id, suffix)) = rest.split_once('/') {
                let op = match suffix {
                    "device-metadata" => DraftOperation::DeviceMetadata,
                    "device-binding" => DraftOperation::DeviceBinding,
                    "motor-calibration" => DraftOperation::MotorCalibration,
                    "module-binding" => DraftOperation::ModuleBinding,
                    "module-calibration" => DraftOperation::ModuleCalibration,
                    "module-migration" => DraftOperation::ModuleMigration,
                    "module-replacement" => DraftOperation::ModuleReplacement,
                    "room" => DraftOperation::Room,
                    "archive" => DraftOperation::Archive,
                    "source" => return Ok(None),
                    _ => return Err("invalid_draft_route".into()),
                };
                (id, op)
            } else {
                (rest, DraftOperation::Save)
            };
            return Uuid::parse_str(id)
                .map(|id| Some(Self::Draft(id, op)))
                .map_err(|_| "invalid_draft_id".into());
        }
        if let Some(id) = path
            .strip_prefix("/api/v1/config/versions/")
            .and_then(|p| p.strip_suffix("/restore-draft"))
        {
            let n = id.parse::<u64>().map_err(|_| "invalid_revision")?;
            if n == 0 || n > i64::MAX as u64 || n.to_string() != id {
                return Err("invalid_revision".into());
            }
            return Ok(Some(Self::RestoreDraft(n)));
        }
        Ok(None)
    }
    pub(crate) fn key(&self) -> String {
        match self {
            Self::CredentialIssue => "/api/v1/credentials".into(),
            Self::CredentialUpdate(id) => format!("PATCH /api/v1/credentials/{id}"),
            Self::CredentialRevoke(id) => format!("/api/v1/credentials/{id}/revoke"),
            Self::RestoreDraft(id) => format!("/api/v1/config/versions/{id}/restore-draft"),
            Self::Draft(id, op) => {
                let suffix = match op {
                    DraftOperation::Save => "",
                    DraftOperation::DeviceMetadata => "/device-metadata",
                    DraftOperation::DeviceBinding => "/device-binding",
                    DraftOperation::MotorCalibration => "/motor-calibration",
                    DraftOperation::ModuleBinding => "/module-binding",
                    DraftOperation::ModuleCalibration => "/module-calibration",
                    DraftOperation::ModuleMigration => "/module-migration",
                    DraftOperation::ModuleReplacement => "/module-replacement",
                    DraftOperation::Room => "/room",
                    DraftOperation::Archive => "/archive",
                };
                format!("/api/v1/config/drafts/{id}{suffix}")
            }
        }
    }
    fn credential(&self) -> bool {
        matches!(
            self,
            Self::CredentialIssue | Self::CredentialUpdate(_) | Self::CredentialRevoke(_)
        )
    }
}
fn response(status: StatusCode, value: Value, expired: bool) -> Response<Full<Bytes>> {
    crate::web_api::receipt_response(status, value, expired)
}
pub(crate) async fn execute(
    api: &Api,
    request: &Request<Bytes>,
    grant: Grant,
    op: Operation,
) -> Response<Full<Bytes>> {
    let error = |status, message| response(status, json!({"error":message}), false);
    if grant.role != Role::Administrator {
        return error(StatusCode::FORBIDDEN, "administrator_required");
    }
    if request
        .headers()
        .get("origin")
        .and_then(|v| v.to_str().ok())
        != Some(&api.origin)
    {
        return error(StatusCode::FORBIDDEN, "origin_required");
    }
    if request.body().len()
        > if op.credential() {
            65536
        } else {
            crate::web_drafts::MAX_BYTES
        }
    {
        return error(
            StatusCode::PAYLOAD_TOO_LARGE,
            "management_request_too_large",
        );
    }
    if request.headers().get_all("idempotency-key").iter().count() != 1 {
        return error(StatusCode::BAD_REQUEST, "idempotency_key_required");
    }
    let Some(key) = request
        .headers()
        .get("idempotency-key")
        .and_then(|v| v.to_str().ok())
    else {
        return error(StatusCode::BAD_REQUEST, "invalid_request_id");
    };
    let mut body = if request.body().is_empty() {
        Value::Null
    } else {
        match serde_json::from_slice::<Value>(request.body()) {
            Ok(b) => b,
            Err(_) => return error(StatusCode::BAD_REQUEST, "invalid_body"),
        }
    };
    // Revalidate requested credential scope before returning a cached record.
    if matches!(
        op,
        Operation::CredentialIssue | Operation::CredentialUpdate(_)
    ) {
        let (scope, expected) = match op {
            Operation::CredentialIssue => {
                match serde_json::from_slice::<crate::web_auth::CredentialRequest>(request.body()) {
                    Ok(r) => {
                        body = json!({"role":r.role,"devices":r.devices,"scenes":r.scenes});
                        (
                            Grant {
                                controller: grant.controller,
                                role: r.role,
                                devices: r.devices,
                                scenes: r.scenes,
                            },
                            None,
                        )
                    }
                    Err(_) => return error(StatusCode::BAD_REQUEST, "invalid_credential_request"),
                }
            }
            _ => {
                match serde_json::from_slice::<crate::web_auth::CredentialUpdate>(request.body()) {
                    Ok(r) => {
                        body = json!({"expected":r.expected,"role":r.role,"devices":r.devices,"scenes":r.scenes});
                        (
                            Grant {
                                controller: grant.controller,
                                role: r.role,
                                devices: r.devices,
                                scenes: r.scenes,
                            },
                            Some(r.expected.controller),
                        )
                    }
                    Err(_) => return error(StatusCode::BAD_REQUEST, "invalid_credential_request"),
                }
            }
        };
        if expected.is_some_and(|id| id != grant.controller)
            || !scope.devices.is_subset(&grant.devices)
            || !scope.scenes.is_subset(&grant.scenes)
        {
            return error(StatusCode::FORBIDDEN, "scope_expansion_forbidden");
        }
        if crate::web_credentials::validate(&scope).is_err() {
            return error(StatusCode::BAD_REQUEST, "invalid_credential_scope");
        }
    }
    // Process boot is not request content. An original receipt remains queryable
    // across restart/configuration change; only a NEW execution validates runtime.
    let digest = Sha256::digest(serde_json::to_vec(&body).unwrap())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    let fingerprint = format!("management-v1:{digest}");
    let endpoint = Endpoint::Management(op.clone());
    let principal = grant.controller.to_string();
    let prior = match api
        .receipts
        .lock()
        .ok()
        .and_then(|r| r.lookup_scoped(&principal, &endpoint, key).ok())
    {
        Some(r) => r,
        None => return error(StatusCode::SERVICE_UNAVAILABLE, "persistence_unavailable"),
    };
    if let Some(record) = prior {
        if record.fingerprint != fingerprint {
            return error(StatusCode::CONFLICT, "idempotency_conflict");
        }
        return cached(record.response, record.expired, key);
    }
    if !op.credential() && api.runtime.read().ok().is_none_or(|r| r.is_none()) {
        return error(StatusCode::SERVICE_UNAVAILABLE, "runtime_unavailable");
    }
    let claim = match api.receipts.lock().ok().and_then(|mut r| {
        r.claim_scoped(&principal, &endpoint, key, &fingerprint)
            .ok()
    }) {
        Some(c) => c,
        None => return error(StatusCode::SERVICE_UNAVAILABLE, "persistence_unavailable"),
    };
    match claim.claim {
        Claim::Conflict => return error(StatusCode::CONFLICT, "idempotency_conflict"),
        Claim::Existing(saved) => return cached(saved, claim.expired, key),
        Claim::New => {}
    }
    let result = api.handle_authorized(request, grant).await;
    let status = result.status();
    let bytes = match result.into_body().collect().await {
        Ok(b) => b.to_bytes(),
        Err(_) => {
            return error(
                StatusCode::SERVICE_UNAVAILABLE,
                "receipt_unavailable_outcome_unknown",
            );
        }
    };
    let original: Value = match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        Err(_) => {
            return error(
                StatusCode::SERVICE_UNAVAILABLE,
                "receipt_unavailable_outcome_unknown",
            );
        }
    };
    let mut saved = original.clone();
    if op == Operation::CredentialIssue
        && let Some(map) = saved.as_object_mut()
    {
        map.remove("secret");
        map.insert("secret_disclosed_once".into(), true.into());
    }
    let record = json!({"http_status":status.as_u16(),"body":saved}).to_string();
    if api
        .receipts
        .lock()
        .ok()
        .and_then(|mut r| r.finish_scoped(&principal, &endpoint, key, &record).ok())
        .is_none()
    {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            "receipt_unavailable_outcome_unknown",
        );
    }
    response(status, original, false)
}

fn cached(saved: Option<String>, expired: bool, key: &str) -> Response<Full<Bytes>> {
    let Some(saved) = saved else {
        return response(
            StatusCode::ACCEPTED,
            json!({"request_id":key,"outcome":"unknown","retry":false}),
            expired,
        );
    };
    let parsed: Value = match serde_json::from_str(&saved) {
        Ok(p) => p,
        Err(_) => {
            return response(
                StatusCode::SERVICE_UNAVAILABLE,
                json!({"error":"receipt_invalid"}),
                expired,
            );
        }
    };
    let status = parsed["http_status"]
        .as_u64()
        .and_then(|n| u16::try_from(n).ok())
        .and_then(|n| StatusCode::from_u16(n).ok())
        .unwrap_or(StatusCode::SERVICE_UNAVAILABLE);
    response(status, parsed["body"].clone(), expired)
}
