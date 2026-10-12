//! Scene action normalization. Authentication and atomic acceptance remain the
//! runtime owner's responsibility, including false writes and subscriptions.
use crate::{
    requests::{RequestError, Subscription},
    scene_graph::SceneBinding,
};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;
#[derive(Debug)]
pub struct SceneBatch {
    pub controller: Uuid,
    /// All addressed scenes, including resets, for permission checks.
    pub addressed: Vec<String>,
    pub starts: Vec<String>,
    /// Explicit values only. A subscription is never an implicit false write.
    pub values: BTreeMap<String, bool>,
    pub subscriptions: Vec<Subscription>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Body {
    characteristics: Vec<Write>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Write {
    aid: u64,
    iid: u64,
    value: Option<serde_json::Value>,
    ev: Option<bool>,
    #[serde(rename = "authData")]
    auth_data: Option<String>,
    #[serde(rename = "remote")]
    _remote: Option<bool>,
}
pub struct SceneRequests {
    bindings: BTreeMap<(u64, u64), String>,
}
impl SceneRequests {
    pub fn new(bindings: Vec<SceneBinding>) -> Result<Self, RequestError> {
        if bindings.is_empty() || bindings.len() > 4095 {
            return Err(RequestError::InvalidBinding);
        }
        let mut result = BTreeMap::new();
        let mut scenes = BTreeSet::new();
        for b in bindings {
            if b.aid <= 1
                || b.on_iid == 0
                || b.scene.trim().is_empty()
                || b.scene.len() > 250
                || b.scene.chars().any(char::is_control)
                || !scenes.insert(b.scene.clone())
                || result.insert((b.aid, b.on_iid), b.scene).is_some()
            {
                return Err(RequestError::InvalidBinding);
            }
        }
        Ok(Self { bindings: result })
    }
    pub fn normalize(&self, controller: Uuid, body: &[u8]) -> Result<SceneBatch, RequestError> {
        if body.len() > 65536 {
            return Err(RequestError::TooLarge);
        }
        let body: Body = serde_json::from_slice(body).map_err(|_| RequestError::InvalidJson)?;
        if body.characteristics.is_empty() {
            return Err(RequestError::Empty);
        }
        if body.characteristics.len() > 128 {
            return Err(RequestError::TooLarge);
        }
        let mut seen = BTreeSet::new();
        let mut batch = SceneBatch {
            controller,
            addressed: vec![],
            starts: vec![],
            values: BTreeMap::new(),
            subscriptions: vec![],
        };
        for w in body.characteristics {
            if !seen.insert((w.aid, w.iid)) {
                return Err(RequestError::Duplicate);
            }
            if w.auth_data.is_some() {
                return Err(RequestError::UnsupportedAuthorization);
            }
            let scene = self
                .bindings
                .get(&(w.aid, w.iid))
                .ok_or(RequestError::UnknownCharacteristic)?;
            if w.value.is_none() && w.ev.is_none() {
                return Err(RequestError::Empty);
            }
            batch.addressed.push(scene.clone());
            if let Some(ev) = w.ev {
                batch.subscriptions.push(Subscription {
                    aid: w.aid,
                    iid: w.iid,
                    enabled: ev,
                });
            }
            if let Some(value) = w.value {
                let on = match value {
                    serde_json::Value::Bool(v) => v,
                    serde_json::Value::Number(n) => match n.as_u64() {
                        Some(0) => false,
                        Some(1) => true,
                        _ => return Err(RequestError::InvalidValue),
                    },
                    _ => return Err(RequestError::InvalidValue),
                };
                batch.values.insert(scene.clone(), on);
                if on {
                    batch.starts.push(scene.clone());
                }
            }
        }
        Ok(batch)
    }
}
