//! Request-level normalization before any queue or device mutation.
//! Controller identity must come from the authenticated HAP session, not JSON.
use homesense_core::{Command, DeviceId, LevelTransition, Percent};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct LightBinding {
    pub device: DeviceId,
    pub aid: u64,
    pub on_iid: u64,
    pub brightness_iid: Option<u64>,
    pub default_on_level: Option<Percent>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestError {
    InvalidBinding,
    TooLarge,
    InvalidJson,
    Empty,
    Duplicate,
    UnknownCharacteristic,
    InvalidValue,
    UnsupportedAuthorization,
    MissingRestoreLevel,
}
impl std::fmt::Display for RequestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for RequestError {}
#[derive(Debug, PartialEq, Eq)]
pub struct Intent {
    pub device: DeviceId,
    pub command: Command,
}
#[derive(Debug, PartialEq, Eq)]
pub struct Subscription {
    pub aid: u64,
    pub iid: u64,
    pub enabled: bool,
}
#[derive(Debug)]
pub struct Batch {
    pub controller: Uuid,
    pub intents: Vec<Intent>,
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
    // A transport hint, never a substitute for session authentication.
    #[serde(rename = "remote")]
    _remote: Option<bool>,
}
#[derive(Clone, Copy)]
enum Field {
    On,
    Brightness,
}
#[derive(Default)]
struct Group {
    on: Option<bool>,
    brightness: Option<Percent>,
}

pub struct LightRequests {
    bindings: BTreeMap<DeviceId, LightBinding>,
    characteristics: BTreeMap<(u64, u64), (DeviceId, Field)>,
}
impl LightRequests {
    pub fn new(bindings: Vec<LightBinding>) -> Result<Self, RequestError> {
        if bindings.is_empty() || bindings.len() > 4096 {
            return Err(RequestError::InvalidBinding);
        }
        let mut result = Self {
            bindings: BTreeMap::new(),
            characteristics: BTreeMap::new(),
        };
        for binding in bindings {
            if binding.aid == 0
                || binding.on_iid == 0
                || binding.brightness_iid == Some(0)
                || binding.default_on_level.is_some_and(|p| p.get() == 0)
                || result.bindings.contains_key(&binding.device)
            {
                return Err(RequestError::InvalidBinding);
            }
            for (iid, field) in std::iter::once((binding.on_iid, Field::On))
                .chain(binding.brightness_iid.map(|i| (i, Field::Brightness)))
            {
                if result
                    .characteristics
                    .insert((binding.aid, iid), (binding.device.clone(), field))
                    .is_some()
                {
                    return Err(RequestError::InvalidBinding);
                }
            }
            result.bindings.insert(binding.device.clone(), binding);
        }
        Ok(result)
    }
    /// `restore_level` reads the last CONFIRMED nonzero level, never desired.
    /// All input validation completes before returning intents. On=false or
    /// Brightness=0 wins within one request, independent of characteristic order.
    /// This result still requires runtime authorization and batch admission.
    pub fn normalize(
        &self,
        controller: Uuid,
        bytes: &[u8],
        mut restore_level: impl FnMut(&DeviceId) -> Option<Percent>,
    ) -> Result<Batch, RequestError> {
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
        let mut groups: BTreeMap<DeviceId, Group> = BTreeMap::new();
        let mut subscriptions = Vec::new();
        for write in body.characteristics {
            if !seen.insert((write.aid, write.iid)) {
                return Err(RequestError::Duplicate);
            }
            if write.auth_data.is_some() {
                return Err(RequestError::UnsupportedAuthorization);
            }
            let (id, field) = self
                .characteristics
                .get(&(write.aid, write.iid))
                .ok_or(RequestError::UnknownCharacteristic)?;
            if write.value.is_none() && write.ev.is_none() {
                return Err(RequestError::Empty);
            }
            if let Some(enabled) = write.ev {
                subscriptions.push(Subscription {
                    aid: write.aid,
                    iid: write.iid,
                    enabled,
                });
            }
            if let Some(value) = write.value {
                let group = groups.entry(id.clone()).or_default();
                match field {
                    Field::On => {
                        group.on = Some(match value {
                            serde_json::Value::Bool(value) => value,
                            serde_json::Value::Number(n) => match n.as_i64() {
                                Some(0) => false,
                                Some(1) => true,
                                _ => return Err(RequestError::InvalidValue),
                            },
                            _ => return Err(RequestError::InvalidValue),
                        })
                    }
                    Field::Brightness => {
                        let level = value
                            .as_u64()
                            .filter(|n| *n <= 100)
                            .ok_or(RequestError::InvalidValue)?;
                        group.brightness = Some(
                            Percent::new(level as u8).map_err(|_| RequestError::InvalidValue)?,
                        );
                    }
                }
            }
        }
        let mut intents = Vec::with_capacity(groups.len());
        for (device, group) in groups {
            let binding = &self.bindings[&device];
            log::debug!(
                "hap_light_fields device={device:?} on={:?} brightness={:?}",
                group.on,
                group.brightness
            );
            let command = if binding.brightness_iid.is_none() {
                Command::Switch(group.on.ok_or(RequestError::InvalidValue)?)
            } else {
                let level =
                    if group.on == Some(false) || group.brightness.is_some_and(|p| p.get() == 0) {
                        Percent::new(0).unwrap()
                    } else if let Some(level) = group.brightness {
                        level
                    } else {
                        restore_level(&device)
                            .filter(|p| p.get() > 0)
                            .or(binding.default_on_level)
                            .ok_or(RequestError::MissingRestoreLevel)?
                    };
                Command::SetLevel {
                    level,
                    transition: LevelTransition::Immediate,
                }
            };
            intents.push(Intent { device, command });
        }
        Ok(Batch {
            controller,
            intents,
            subscriptions,
        })
    }
}
