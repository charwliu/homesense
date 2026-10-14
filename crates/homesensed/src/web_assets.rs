//! Exact compiled script versions; no generic URI-query exception.
use http_body_util::Full;
use hyper::{Response, StatusCode, body::Bytes};
use sha2::{Digest, Sha256};
use std::sync::OnceLock;
#[derive(Clone, Copy)]
pub(crate) struct Script {
    pub path: &'static str,
    bytes: &'static [u8],
    version: &'static str,
}
fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
pub(crate) fn script(path: &str) -> Option<Script> {
    static APP: OnceLock<String> = OnceLock::new();
    static DRAFT: OnceLock<String> = OnceLock::new();
    let (path, bytes, version) = match path {
        "/app.js" => (
            "/app.js",
            include_bytes!("web/app.js").as_slice(),
            APP.get_or_init(|| digest(include_bytes!("web/app.js")))
                .as_str(),
        ),
        "/draft-json.js" => (
            "/draft-json.js",
            include_bytes!("web/draft-json.js").as_slice(),
            DRAFT
                .get_or_init(|| digest(include_bytes!("web/draft-json.js")))
                .as_str(),
        ),
        _ => return None,
    };
    Some(Script {
        path,
        bytes,
        version,
    })
}
impl Script {
    pub fn validate_query(self, query: Option<&str>) -> Result<(), StatusCode> {
        let Some(query) = query else {
            return Ok(());
        };
        let version = query.strip_prefix("v=").ok_or(StatusCode::BAD_REQUEST)?;
        if version.len() != 64
            || !version
                .bytes()
                .all(|b| b.is_ascii_digit() || matches!(b, b'a'..=b'f'))
        {
            return Err(StatusCode::BAD_REQUEST);
        }
        if version != self.version {
            return Err(StatusCode::CONFLICT);
        }
        Ok(())
    }
    pub fn response(self) -> Response<Full<Bytes>> {
        Response::builder()
            .header("content-type", "text/javascript; charset=utf-8")
            .header("cache-control", "no-store")
            .header("x-content-type-options", "nosniff")
            .header("x-homesense-asset-sha256", self.version)
            .body(Full::new(Bytes::from_static(self.bytes)))
            .unwrap()
    }
}
pub(crate) fn index() -> Bytes {
    let mut html = include_str!("web/index.html").to_owned();
    for path in ["/draft-json.js", "/app.js"] {
        let script = script(path).unwrap();
        html = html.replace(
            &format!("src=\"{}\"", script.path),
            &format!("src=\"{}?v={}\"", script.path, script.version),
        );
    }
    Bytes::from(html)
}
