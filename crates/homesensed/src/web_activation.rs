//! Administrative publication requests use the same durable no-replay fence as controls.
use crate::{
    web_api::{Api, Runtime},
    web_auth::{Grant, Role},
    web_receipts::Claim,
};
use hyper::{Request, StatusCode, body::Bytes};
use serde_json::{Value, json};
use uuid::Uuid;
pub(crate) async fn activate_scoped(
    api: &Api,
    runtime: &Runtime,
    grant: &Grant,
    id: &str,
    request: &Request<Bytes>,
) -> (StatusCode, Value, bool) {
    let error = |status, message| (status, json!({"error":message}),false);
    if grant.role != Role::Administrator {
        return error(StatusCode::FORBIDDEN, "administrator_required");
    }
    if request.method() != hyper::Method::POST {
        return error(StatusCode::METHOD_NOT_ALLOWED, "method_not_allowed");
    }
    if request
        .headers()
        .get("origin")
        .and_then(|v| v.to_str().ok())
        != Some(&api.origin)
    {
        return error(StatusCode::FORBIDDEN, "origin_required");
    }
    if request
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        != Some("application/json")
        || request.body().len() > 4096
    {
        return error(StatusCode::BAD_REQUEST, "invalid_body");
    }
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Input {
        request_id: String,
        boot: String,
    }
    let Ok(input) = serde_json::from_slice::<Input>(request.body()) else {
        return error(StatusCode::BAD_REQUEST, "invalid_body");
    };
    let Ok(id) = Uuid::parse_str(id) else {
        return error(StatusCode::BAD_REQUEST, "invalid_draft_id");
    };
    let Some(publisher) = &api.publisher else {
        return error(StatusCode::SERVICE_UNAVAILABLE, "publication_unavailable");
    };
    let Some((_, drafts)) = &api.drafts else {
        return error(StatusCode::SERVICE_UNAVAILABLE, "drafts_unavailable");
    };
    let draft = match drafts
        .lock()
        .map_err(|_| ())
        .and_then(|store| store.get(grant.controller, id).map_err(|_| ()))
    {
        Ok(Some(draft)) => draft,
        Ok(None) => return error(StatusCode::NOT_FOUND, "draft_not_found"),
        Err(_) => return error(StatusCode::SERVICE_UNAVAILABLE, "draft_storage_unavailable"),
    };
    let principal = grant.controller.to_string();
    let endpoint=crate::web_receipts::Endpoint::ConfigurationActivate(id);
    let fingerprint=json!({"action":"activate","draft":id,"revision":draft.revision,"expected_revision":draft.expected_revision,"boot":input.boot}).to_string();
    // Resolve earlier receipts before current-revision checks: a completed
    // publication remains queryable after its revision replaces the old one.
    let existing = api
        .receipts
        .lock()
        .map_err(|_| ())
        .and_then(|r| r.lookup_scoped(&principal,&endpoint,&input.request_id).map_err(|_| ()));
    match existing {
        Ok(Some(record)) => {
            let (old,result,expired)=(record.fingerprint,record.response,record.expired);
            if old != fingerprint {
                return error(StatusCode::CONFLICT, "idempotency_conflict");
            }
            return (
                StatusCode::OK,
                result
                    .and_then(|r| serde_json::from_str(&r).ok())
                    .unwrap_or(json!({"outcome":"unknown","retry":false})),
                expired,
            );
        }
        Err(_) => return error(StatusCode::SERVICE_UNAVAILABLE, "persistence_unavailable"),
        _ => {}
    }
    if runtime.revision != draft.expected_revision || runtime.boot != input.boot {
        return error(StatusCode::CONFLICT, "configuration_changed");
    }
    let claim = api.receipts.lock().map_err(|_| ()).and_then(|mut r| {
        r.claim_scoped(&principal,&endpoint,&input.request_id,&fingerprint)
            .map_err(|_| ())
    });
    let claim=match claim{Ok(c)=>c,Err(_)=>return error(StatusCode::SERVICE_UNAVAILABLE,"persistence_unavailable")};
    let expired=claim.expired;
    match claim.claim {
        Claim::New => {}
        Claim::Existing(Some(value)) => {
            return (
                StatusCode::OK,
                serde_json::from_str(&value).unwrap_or(json!({"outcome":"unknown","retry":false})),
                expired,
            );
        }
        Claim::Existing(None) => {
            return (
                StatusCode::ACCEPTED,
                json!({"outcome":"unknown","retry":false}),
                expired,
            );
        }
        Claim::Conflict => return error(StatusCode::CONFLICT, "idempotency_conflict"),
    }
    let outcome = match publisher
        .publish(
            serde_json::to_vec(&draft.devices).unwrap(),
            serde_json::to_vec(&draft.automation).unwrap(),
            draft.expected_revision,
        )
        .await
    {
        Ok(revision) => {
            json!({"request_id":input.request_id,"outcome":"activated","revision":revision})
        }
        Err(_) => json!({"request_id":input.request_id,"outcome":"unknown","retry":false}),
    };
    if api
        .receipts
        .lock()
        .map_err(|_| ())
        .and_then(|mut r| {
            r.finish_scoped(&principal,&endpoint,&input.request_id,&outcome.to_string())
                .map_err(|_| ())
        })
        .is_err()
    {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            "receipt_unavailable_outcome_unknown",
        );
    }
    (StatusCode::ACCEPTED, outcome,expired)
}

/// Compatibility wrapper; actual HTTP additionally returns expiry headers.
pub async fn activate(api:&Api,runtime:&Runtime,grant:&Grant,id:&str,request:&Request<Bytes>)->(StatusCode,Value){let(code,value,_)=activate_scoped(api,runtime,grant,id,request).await;(code,value)}
