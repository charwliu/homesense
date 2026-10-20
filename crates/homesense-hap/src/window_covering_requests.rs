//! Validate native covering writes before admitting any motor command.
use crate::{
    requests::{RequestError, Subscription},
    window_covering_graph::WindowCoveringBinding,
};
use homesense_core::DeviceId;
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WindowCommand {
    Target { device: DeviceId, percent: u8 },
    Hold { device: DeviceId },
}

#[derive(Debug)]
pub struct WindowBatch {
    pub controller: Uuid,
    pub addressed: Vec<DeviceId>,
    pub command: Option<WindowCommand>,
    pub subscriptions: Vec<Subscription>,
}

#[derive(Clone, Copy)]
enum Field {
    Current,
    State,
    Target,
    Hold,
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

pub struct WindowCoveringRequests {
    fields: BTreeMap<(u64, u64), (DeviceId, Field)>,
}

impl WindowCoveringRequests {
    pub fn new(bindings: Vec<WindowCoveringBinding>) -> Result<Self, RequestError> {
        if bindings.is_empty() || bindings.len() > 4095 {
            return Err(RequestError::InvalidBinding);
        }
        let mut fields = BTreeMap::new();
        let mut devices = BTreeSet::new();
        for binding in bindings {
            if binding.aid <= 1 || !devices.insert(binding.device.clone()) {
                return Err(RequestError::InvalidBinding);
            }
            for (iid, field) in [
                (binding.current_iid, Field::Current),
                (binding.state_iid, Field::State),
                (binding.target_iid, Field::Target),
                (binding.hold_iid, Field::Hold),
            ] {
                if iid == 0
                    || fields
                        .insert((binding.aid, iid), (binding.device.clone(), field))
                        .is_some()
                {
                    return Err(RequestError::InvalidBinding);
                }
            }
        }
        Ok(Self { fields })
    }

    pub fn normalize(&self, controller: Uuid, bytes: &[u8]) -> Result<WindowBatch, RequestError> {
        if bytes.len() > 65536 {
            return Err(RequestError::TooLarge);
        }
        let body: Body = serde_json::from_slice(bytes).map_err(|_| RequestError::InvalidJson)?;
        if body.characteristics.is_empty() {
            return Err(RequestError::Empty);
        }
        if body.characteristics.len() > 128 {
            return Err(RequestError::TooLarge);
        }
        let mut seen = BTreeSet::new();
        let mut addressed = BTreeSet::new();
        let mut command = None;
        let mut subscriptions = Vec::new();
        for write in body.characteristics {
            if !seen.insert((write.aid, write.iid)) {
                return Err(RequestError::Duplicate);
            }
            if write.auth_data.is_some() {
                return Err(RequestError::UnsupportedAuthorization);
            }
            let (device, field) = self
                .fields
                .get(&(write.aid, write.iid))
                .ok_or(RequestError::UnknownCharacteristic)?;
            if write.value.is_none() && write.ev.is_none() {
                return Err(RequestError::Empty);
            }
            addressed.insert(device.clone());
            if let Some(enabled) = write.ev {
                if matches!(field, Field::Hold) {
                    return Err(RequestError::InvalidValue);
                }
                subscriptions.push(Subscription {
                    aid: write.aid,
                    iid: write.iid,
                    enabled,
                });
            }
            let Some(value) = write.value else { continue };
            let next = match field {
                Field::Target => WindowCommand::Target {
                    device: device.clone(),
                    percent: value
                        .as_u64()
                        .filter(|n| *n <= 100)
                        .ok_or(RequestError::InvalidValue)? as u8,
                },
                Field::Hold => {
                    if value == serde_json::Value::Bool(false) {
                        continue;
                    }
                    if value != serde_json::Value::Bool(true) {
                        return Err(RequestError::InvalidValue);
                    }
                    WindowCommand::Hold {
                        device: device.clone(),
                    }
                }
                Field::Current | Field::State => return Err(RequestError::InvalidValue),
            };
            if command.replace(next).is_some() {
                return Err(RequestError::InvalidValue);
            }
        }
        Ok(WindowBatch {
            controller,
            addressed: addressed.into_iter().collect(),
            command,
            subscriptions,
        })
    }
}
