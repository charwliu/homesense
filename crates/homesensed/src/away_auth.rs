//! Source-specific Away authorization. HAP never borrows a Web credential.
use crate::web_auth::ModuleOffLease;
use homesense_core::queue::Origin;
use homesense_hap::storage::IdentityStore;
use std::{
    collections::BTreeSet,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use uuid::Uuid;
#[derive(Clone)]
pub struct HapLease {
    identity: IdentityStore,
    controller: Uuid,
    public_key: [u8; 32],
    devices: BTreeSet<String>,
    active: Arc<AtomicBool>,
}
impl std::fmt::Debug for HapLease {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("HapAwayLease(<opaque>)")
    }
}
impl HapLease {
    pub(crate) fn new(
        identity: IdentityStore,
        controller: Uuid,
        devices: BTreeSet<String>,
        active: Arc<AtomicBool>,
    ) -> Option<Self> {
        let public_key = identity.controller_public_key(controller).ok()??;
        Some(Self {
            identity,
            controller,
            public_key,
            devices,
            active,
        })
    }
    fn valid(&self) -> bool {
        self.active.load(Ordering::Acquire)
            && self
                .identity
                .controller_public_key(self.controller)
                .ok()
                .flatten()
                == Some(self.public_key)
    }
}
#[derive(Clone,Debug,serde::Serialize,serde::Deserialize)]
#[serde(tag="action",rename_all="snake_case",deny_unknown_fields)]
pub(crate) enum KeypadCommand {KeypadPress{keypad:String,key:u8}}
#[derive(Clone)]
pub struct KeypadWebLease {source:ModuleOffLease,parent:homesense_core::DeviceId,key:u8}
impl std::fmt::Debug for KeypadWebLease {fn fmt(&self,f:&mut std::fmt::Formatter<'_>)->std::fmt::Result {f.write_str("KeypadAwayLease(<opaque>)")}}
#[derive(Clone,Debug)]
pub struct KeypadPanelLease {source:crate::panel_execution_auth::Lease,parent:homesense_core::DeviceId,key:u8}
#[derive(Clone,Debug)]
pub(crate) struct KeypadBinding {pub endpoint:homesense_core::DeviceId,pub reset_green:bool}
#[derive(Clone, Debug)]
pub enum Authorization {
    Web(ModuleOffLease),
    KeypadWeb(KeypadWebLease),
    KeypadPanel(KeypadPanelLease),
    Hap(HapLease),
}
impl From<ModuleOffLease> for Authorization {
    fn from(lease: ModuleOffLease) -> Self {
        Self::Web(lease)
    }
}
impl Authorization {
    pub(crate) fn same_authority(&self,other:&Self)->bool{match(self,other){
        (Self::Web(a),Self::Web(b))=>a.same_authority(b),
        (Self::KeypadWeb(a),Self::KeypadWeb(b))=>a.parent==b.parent&&a.key==b.key&&a.source.same_authority(&b.source),
        (Self::KeypadPanel(a),Self::KeypadPanel(b))=>a.parent==b.parent&&a.key==b.key&&a.source.same_authority(&b.source),
        (Self::Hap(a),Self::Hap(b))=>a.identity.same_owner(&b.identity)&&a.controller==b.controller&&a.public_key==b.public_key&&a.devices==b.devices&&Arc::ptr_eq(&a.active,&b.active),
        _=>false,
    }}

    pub(crate) fn keypad(source:ModuleOffLease,command:&KeypadCommand)->Option<Self> {
        let KeypadCommand::KeypadPress{keypad,key}=command;
        if !(1..=6).contains(key) || !source.valid() {return None;}
        Some(Self::KeypadWeb(KeypadWebLease{source,parent:homesense_core::DeviceId::new(keypad).ok()?,key:*key}))
    }
    pub(crate) fn configured_keypad(source:crate::panel_execution_auth::Lease,keypad:String,key:u8)->Option<Self>{
        if !(1..=6).contains(&key)||!source.valid(){return None;}
        Some(Self::KeypadPanel(KeypadPanelLease{source,parent:homesense_core::DeviceId::new(keypad).ok()?,key}))
    }
    pub(crate) fn needs_configured_source(&self)->bool{matches!(self,Self::KeypadPanel(_))}
    pub(crate) fn key_binding(&self,config:&homesense_config::Configuration)->Result<Option<KeypadBinding>,String> {
        let (parent,number,devices)=match self {
            Self::KeypadWeb(lease) if lease.source.valid()=>(&lease.parent,lease.key,lease.source.devices()),
            Self::KeypadPanel(lease) if lease.source.valid()=>(&lease.parent,lease.key,lease.source.devices()),
            Self::KeypadWeb(_)|Self::KeypadPanel(_)=>return Err("Away key source expired".into()),
            _=>return Ok(None),
        };
        let key=config.keypads().get(parent).filter(|p|p.enabled).and_then(|p|p.keys.get(&number)).filter(|k|k.led_endpoint.enabled).ok_or("Away key unavailable")?;
        let Some(homesense_config::keypad::KeyAction::AwayOff{indicator_reset,..})=&key.action else {return Err("Away key mapping changed".into());};
        let endpoint=homesense_core::DeviceId::new(&key.led_endpoint.id).map_err(|e|e.to_string())?;
        if !devices.contains(endpoint.as_str()) || config.binding(&endpoint).is_none() {return Err("Away key not authorized".into());}
        Ok(Some(KeypadBinding{endpoint,reset_green:indicator_reset.is_some()}))
    }
    pub(crate) fn indicator_lease(&self,config:&homesense_config::Configuration)->Result<Option<(homesense_core::DeviceId,crate::panel_auth::Lease)>,String> {
        let Self::KeypadWeb(source)=self else {return Ok(None);};
        let Some(key)=self.key_binding(config)?.filter(|key|key.reset_green) else {return Ok(None);};
        let lease=crate::panel_auth::Lease::led(source.source.clone(),config,key.endpoint.clone(),homesense_core::Command::PanelLed(false)).ok_or("Away key green reset not authorized")?;
        Ok(Some((key.endpoint,lease)))
    }
    pub(crate) fn with_valid<T>(&self,operation:impl FnOnce()->T)->Option<T> {
        match self {Self::Web(source)=>source.with_valid(operation),Self::KeypadWeb(source)=>source.source.with_valid(operation),Self::KeypadPanel(source)=>source.source.with_valid(operation),Self::Hap(source)=>source.valid().then(operation)}
    }
    pub fn controller(&self) -> Uuid {
        match self {
            Self::Web(lease) => lease.controller(),
            Self::KeypadWeb(lease)=>lease.source.controller(),
            Self::KeypadPanel(lease)=>lease.source.controller(),
            Self::Hap(lease) => lease.controller,
        }
    }
    pub fn devices(&self) -> &BTreeSet<String> {
        match self {
            Self::Web(lease) => lease.devices(),
            Self::KeypadWeb(lease)=>lease.source.devices(),
            Self::KeypadPanel(lease)=>lease.source.devices(),
            Self::Hap(lease) => &lease.devices,
        }
    }
    pub(crate) fn valid(&self) -> bool {
        match self {
            Self::Web(lease) => lease.valid(),
            Self::KeypadWeb(lease)=>lease.source.valid(),
            Self::KeypadPanel(lease)=>lease.source.valid(),
            Self::Hap(lease) => lease.valid(),
        }
    }
    pub(crate) fn origin(&self) -> Origin {
        match self {
            Self::Web(_) | Self::KeypadWeb(_) => Origin::Http,
            Self::KeypadPanel(_)=>Origin::Rule,
            Self::Hap(_) => Origin::Hap,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hap::{
        pairing::{Pairing, Permissions},
        storage::Storage,
    };
    #[tokio::test]
    async fn hap_capability_is_bridge_local_key_bound_revocable_and_retired() {
        let root = std::env::temp_dir().join(format!("away-auth-{}", Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let mut identity = IdentityStore::open(root.join("first")).await.unwrap();
        identity.save_config(&hap::Config::default()).await.unwrap();
        let mut other = IdentityStore::open(root.join("second")).await.unwrap();
        other.save_config(&hap::Config::default()).await.unwrap();
        let controller = Uuid::new_v4();
        let active = Arc::new(AtomicBool::new(true));
        let devices = BTreeSet::from(["synthetic:1".into()]);
        assert!(
            HapLease::new(
                identity.clone(),
                controller,
                devices.clone(),
                active.clone()
            )
            .is_none()
        );
        identity
            .save_pairing(&Pairing::new(controller, Permissions::User, [5; 32]))
            .await
            .unwrap();
        assert!(
            HapLease::new(other.clone(), controller, devices.clone(), active.clone()).is_none()
        );
        let lease = HapLease::new(
            identity.clone(),
            controller,
            devices.clone(),
            active.clone(),
        )
        .unwrap();
        assert!(lease.valid());
        identity.delete_pairing(&controller).await.unwrap();
        assert!(!lease.valid());
        identity
            .save_pairing(&Pairing::new(controller, Permissions::User, [6; 32]))
            .await
            .unwrap();
        assert!(!lease.valid());
        let replacement =
            HapLease::new(identity.clone(), controller, devices, active.clone()).unwrap();
        assert!(replacement.valid());
        active.store(false, Ordering::Release);
        assert!(!replacement.valid());
        drop((identity, other, lease, replacement));
        std::fs::remove_dir_all(root).unwrap();
    }
}
