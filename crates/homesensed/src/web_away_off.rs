//! Explicit, scoped and durable Away operation; no device list from the client.
use crate::{
    automation_control::{Action, Reply},
    web_api::{Api, Runtime},
    web_auth::Grant,
    web_receipts::{Claim, Endpoint},
};
use http_body_util::Full;
use hyper::{Request, Response, StatusCode, body::Bytes};
use serde_json::json;
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Body {
    revision: String,
    request_id: String,
    #[serde(default)]
    command:Option<crate::away_auth::KeypadCommand>,
}
pub(crate) async fn execute(
    api: &Api,
    runtime: &Runtime,
    grant: &Grant,
    request: &Request<Bytes>,
) -> Response<Full<Bytes>> {
    let response = |code, value| crate::web_api::receipt_response(code, value, false);
    if !matches!(*request.method(), hyper::Method::POST | hyper::Method::GET) {
        return response(
            StatusCode::METHOD_NOT_ALLOWED,
            json!({"error":"method_not_allowed"}),
        );
    }
    let read = request.method() == hyper::Method::GET;
    if request.uri().query().is_some()
        || request.body().len() > 1024
        || (read && !request.body().is_empty())
        || (!read
            && (request
                .headers()
                .get("origin")
                .and_then(|v| v.to_str().ok())
                != Some(&api.origin)
                || request
                    .headers()
                    .get("content-type")
                    .and_then(|v| v.to_str().ok())
                    != Some("application/json")))
    {
        return response(
            StatusCode::BAD_REQUEST,
            json!({"error":"invalid_away_request"}),
        );
    }
    let lease = api.tokens.module_off_lease(
        request
            .headers()
            .get("authorization")
            .and_then(|v| v.to_str().ok()),
        request
            .headers()
            .get("cookie")
            .and_then(|v| v.to_str().ok())
            .and_then(crate::web_api::session_cookie),
        request
            .headers()
            .get("x-csrf-token")
            .and_then(|v| v.to_str().ok()),
    );
    let Some(lease) = lease.filter(|lease| lease.controller() == grant.controller) else {
        return response(StatusCode::FORBIDDEN, json!({"error":"away_not_permitted"}));
    };
    let body=if read {None} else {
        let parsed=match serde_json::from_slice::<Body>(request.body()) {Ok(body)=>body,Err(_)=>return response(StatusCode::BAD_REQUEST,json!({"error":"invalid_body"}))};
        if parsed.revision!=runtime.revision.to_string() {return response(StatusCode::CONFLICT,json!({"error":"stale_revision"}));}
        Some(parsed)
    };
    let lease:crate::away_auth::Authorization=if let Some(command)=body.as_ref().and_then(|body|body.command.as_ref()) {
        let Some(lease)=crate::away_auth::Authorization::keypad(lease,command) else {return response(StatusCode::BAD_REQUEST,json!({"error":"invalid_keypad_press"}));};lease
    }else{lease.into()};
    let available = matches!(
        runtime
            .control
            .request(Action::AwayOff {
                controller: grant.controller,
                revision: runtime.revision,
                lease: lease.clone(),
                submit: false
            })
            .await,
        Ok(Reply::CommandsAccepted(_))
    );
    if read {
        let operations=if available {match runtime.control.request(Action::AwayOperations{controller:grant.controller,revision:runtime.revision,lease}).await {Ok(Reply::AwayOperations(operations))=>operations,_=>vec![]}}else{vec![]};
        return response(
            StatusCode::OK,
            json!({"available":available,"revision":runtime.revision.to_string(),"operations":operations}),
        );
    }
    if !available {
        return response(
            StatusCode::FORBIDDEN,
            json!({"error":"away_scope_not_permitted_or_unavailable"}),
        );
    }
    let body=body.expect("validated non-read body");
    let principal = grant.controller.to_string();
    let endpoint = Endpoint::AwayOff;
    let mut fingerprint=json!({"revision":runtime.revision,"action":"away_module_all_off_immediate"});
    if let Some(command)=&body.command {fingerprint["command"]=json!(command);}
    let fingerprint=fingerprint.to_string();
    let claim = api.receipts.lock().ok().and_then(|mut r| {
        r.claim_scoped(&principal, &endpoint, &body.request_id, &fingerprint)
            .ok()
    });
    let Some(claim) = claim else {
        return response(
            StatusCode::SERVICE_UNAVAILABLE,
            json!({"error":"receipt_unavailable"}),
        );
    };
    let expired = claim.expired;
    match claim.claim {
        Claim::Existing(Some(value)) => {
            return crate::web_api::receipt_response(
                StatusCode::OK,
                serde_json::from_str(&value).unwrap_or(json!({"outcome":"unknown","retry":false})),
                expired,
            );
        }
        Claim::Existing(None) => {
            return crate::web_api::receipt_response(
                StatusCode::ACCEPTED,
                json!({"request_id":body.request_id,"outcome":"unknown","retry":false}),
                expired,
            );
        }
        Claim::Conflict => {
            return response(
                StatusCode::CONFLICT,
                json!({"error":"idempotency_conflict"}),
            );
        }
        Claim::New => {}
    }
    let outcome = match runtime
        .control
        .request(Action::AwayOff {
            controller: grant.controller,
            revision: runtime.revision,
            lease,
            submit: true,
        })
        .await
    {
        Ok(Reply::AwayAccepted(accepted))=>json!({"request_id":body.request_id,"outcome":"accepted","boot":runtime.boot,"operation":accepted.operation,"commands":accepted.commands,"physical_result_confirmed":false,"retry":false,"server_retry":{"max_rounds":3,"delay_ms":1000}}),
        Ok(Reply::CommandsAccepted(commands)) => {
            json!({"request_id":body.request_id,"outcome":"accepted","boot":runtime.boot,"commands":commands,"physical_result_confirmed":false,"retry":false})
        }
        _ => json!({"request_id":body.request_id,"outcome":"unknown","retry":false}),
    };
    if !api.receipts.lock().ok().is_some_and(|mut r| {
        r.finish_scoped(
            &principal,
            &endpoint,
            &body.request_id,
            &outcome.to_string(),
        )
        .is_ok()
    }) {
        return response(
            StatusCode::SERVICE_UNAVAILABLE,
            json!({"error":"receipt_unavailable_outcome_unknown"}),
        );
    }
    response(StatusCode::ACCEPTED, outcome)
}
