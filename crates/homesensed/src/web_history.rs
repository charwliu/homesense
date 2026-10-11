//! Administrator-only local configuration history. No publication or bus writes.
use crate::{
    automation_control::{Action, Reply},
    web_api::Runtime,
    web_auth::{Grant, Role},
};
use hyper::{Request, StatusCode, body::Bytes};
use serde_json::{Value, json};
pub async fn read(
    runtime: &Runtime,
    grant: &Grant,
    request: &Request<Bytes>,
) -> (StatusCode, Value) {
    let error = |code, message| (code, json!({"error":message}));
    if grant.role != Role::Administrator {
        return error(StatusCode::FORBIDDEN, "administrator_required");
    }
    if request.method() != hyper::Method::GET {
        return error(StatusCode::METHOD_NOT_ALLOWED, "method_not_allowed");
    }
    if !request.body().is_empty() {
        return error(StatusCode::BAD_REQUEST, "unexpected_body");
    }
    if request.uri().path() == "/api/v1/config/versions" {
        let Ok((before, limit)) = page(request.uri().query()) else {
            return error(StatusCode::BAD_REQUEST, "invalid_page");
        };
        match runtime
            .control
            .request(Action::ConfigurationHistory {
                controller: grant.controller,
                revision: runtime.revision,
                before,
                limit,
            })
            .await
        {
            Ok(Reply::ConfigurationHistory(page)) => (
                StatusCode::OK,
                json!({"boot":runtime.boot,"active_revision":page.active_revision.map(|v|v.to_string()),"next_before":page.next_before.map(|v|v.to_string()),"versions":page.revisions.iter().map(|r|json!({"revision":r.revision.to_string(),"device_bytes":r.device_bytes,"automation_bytes":r.automation_bytes})).collect::<Vec<_>>()}),
            ),
            _ => error(
                StatusCode::SERVICE_UNAVAILABLE,
                "configuration_history_unavailable",
            ),
        }
    } else {
        if request.uri().query().is_some() {
            return error(StatusCode::BAD_REQUEST, "unexpected_query");
        }
        let selected = request
            .uri()
            .path()
            .strip_prefix("/api/v1/config/versions/")
            .and_then(|s| s.strip_suffix("/export"))
            .and_then(number);
        let Some(selected) = selected else {
            return error(StatusCode::BAD_REQUEST, "invalid_revision");
        };
        match runtime
            .control
            .request(Action::ConfigurationExport {
                controller: grant.controller,
                revision: runtime.revision,
                selected,
            })
            .await
        {
            Ok(Reply::ConfigurationExport(Some(export))) => {
                let documents = (|| {
                    let devices: Value = serde_json::from_slice(&export.devices).ok()?;
                    let automation = export
                        .automation
                        .as_ref()
                        .map(|v| serde_json::from_slice::<Value>(v))
                        .transpose()
                        .ok()?;
                    Some(
                        json!({"boot":runtime.boot,"active_revision":runtime.revision.to_string(),"revision":export.revision.to_string(),"devices":devices,"automation":automation}),
                    )
                })();
                documents.map(|v| (StatusCode::OK, v)).unwrap_or_else(|| {
                    error(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "configuration_export_unavailable",
                    )
                })
            }
            Ok(Reply::ConfigurationExport(None)) => {
                error(StatusCode::NOT_FOUND, "revision_not_found")
            }
            _ => error(
                StatusCode::SERVICE_UNAVAILABLE,
                "configuration_export_unavailable",
            ),
        }
    }
}
fn number(value: &str) -> Option<u64> {
    if value.is_empty() || value.len() > 19 || !value.bytes().all(|v| v.is_ascii_digit()) {
        return None;
    }
    value
        .parse::<u64>()
        .ok()
        .filter(|n| *n > 0 && *n <= i64::MAX as u64)
}
fn page(query: Option<&str>) -> Result<(Option<u64>, usize), ()> {
    let Some(query) = query else {
        return Ok((None, 20));
    };
    if query.is_empty() || query.len() > 100 {
        return Err(());
    }
    let mut before = None;
    let mut limit = None;
    for pair in query.split('&') {
        let (key, value) = pair.split_once('=').ok_or(())?;
        let value = number(value).ok_or(())?;
        match key {
            "before" if before.is_none() => before = Some(value),
            "limit" if limit.is_none() && value <= 100 => limit = Some(value as usize),
            _ => return Err(()),
        }
    }
    Ok((before, limit.unwrap_or(20)))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pagination_preserves_full_sql_integer_range() {
        assert_eq!(page(Some("before=9223372036854775807&limit=100")),Ok((Some(i64::MAX as u64),100)));
        for query in ["before=9223372036854775808","before=1&before=2","limit=0","limit=101","limit=1&token=secret","before=%31",""] {assert!(page(Some(query)).is_err());}
    }
}
