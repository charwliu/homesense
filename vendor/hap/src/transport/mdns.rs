use crate::{pointer, Config, Error, Result};
use futures::{future::BoxFuture, FutureExt};
use mdns_sd::{DaemonEvent, IfKind, ServiceDaemon, ServiceInfo};
use std::{collections::HashMap, io};

fn failure(error: impl std::fmt::Display) -> Error {
    io::Error::other(format!("mDNS: {error}")).into()
}

fn service_info(config: &Config) -> Result<ServiceInfo> {
    if config.host.is_unspecified() || config.host.is_multicast() || config.port == 0 {
        return Err(failure("advertising requires a concrete listening address and port"));
    }
    let hostname = format!("homesense-{}.local.", config.device_id.to_string().replace(':', "").to_lowercase());
    let txt: HashMap<String, String> = config.txt_records().iter().map(|item| {
        let (key, value) = item.split_once('=').expect("fixed HAP TXT format");
        (key.to_owned(), value.to_owned())
    }).collect();
    let mut service = ServiceInfo::new("_hap._tcp.local.", &config.name, &hostname, config.host, config.port, txt).map_err(failure)?;
    service.set_interfaces(vec![IfKind::Addr(config.host)]);
    // Do not enable address auto-detection: the TCP listener accepts this IP only.
    Ok(service)
}

pub struct MdnsResponder {
    config: pointer::Config,
    daemon: Option<ServiceDaemon>,
    events: Option<mdns_sd::Receiver<DaemonEvent>>,
    fullname: Option<String>,
}
impl MdnsResponder {
    pub async fn new(config: pointer::Config) -> Result<Self> {
        let host = {
            let current = config.lock().await;
            service_info(&current)?;
            current.host
        };
        let mut result = Self { config, daemon: Some(ServiceDaemon::new().map_err(failure)?), events: None, fullname: None };
        let daemon = result.daemon.as_ref().unwrap();
        result.events = Some(daemon.monitor().map_err(failure)?);
        daemon.disable_interface(IfKind::All).map_err(failure)?;
        daemon.enable_interface(IfKind::Addr(host)).map_err(failure)?;
        Ok(result)
    }
    pub fn disabled(config: pointer::Config) -> Self {
        Self { config, daemon: None, events: None, fullname: None }
    }
    pub async fn update_records(&mut self) -> Result<()> {
        if let Some(daemon) = &self.daemon {
            let info = service_info(&*self.config.lock().await)?;
            let fullname = info.get_fullname().to_owned();
            if let Some(old) = &self.fullname {
                if *old != fullname { daemon.unregister(old).map_err(failure)?; }
            }
            daemon.register(info).map_err(failure)?;
            self.fullname = Some(fullname);
        }
        Ok(())
    }
    pub fn run_handle(&mut self) -> BoxFuture<'static, Result<()>> {
        match &self.events {
            None => futures::future::pending().boxed(),
            Some(events) => {
                let events = events.clone();
                async move {
                    while let Ok(event) = events.recv_async().await {
                        match event {
                            DaemonEvent::Error(error) => return Err(failure(error)),
                            other => log::debug!("mDNS event: {:?}", other),
                        }
                    }
                    Err(failure("daemon event channel closed"))
                }.boxed()
            }
        }
    }
}
impl Drop for MdnsResponder {
    fn drop(&mut self) {
        if let Some(daemon) = &self.daemon {
            if let Some(name) = &self.fullname { let _ = daemon.unregister(name); }
            let _ = daemon.shutdown();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn advertisement_contains_only_listener_address_and_stable_identity() {
        for host in ["192.168.4.1", "fd12::1"] {
            let mut config = Config::default();
            config.host = host.parse().unwrap();
            config.name = "HomeSense Simulation".into();
            let info = service_info(&config).unwrap();
            assert_eq!(info.get_addresses().len(), 1);
            assert!(info.get_addresses().contains(&config.host));
            assert!(!info.is_addr_auto());
            assert_eq!(info.get_port(), config.port);
            assert_eq!(info.get_property_val_str("id"), Some(config.device_id.to_string().as_str()));
            let hostname = info.get_hostname().to_owned();
            config.configuration_number += 1;
            let updated = service_info(&config).unwrap();
            assert_eq!(updated.get_hostname(), hostname);
            assert_eq!(updated.get_property_val_str("c#"), Some(config.configuration_number.to_string().as_str()));
        }
    }
    #[test]
    fn wildcard_or_multicast_listener_cannot_be_advertised() {
        for host in ["0.0.0.0", "::", "224.0.0.251", "ff02::fb"] {
            let mut config = Config::default(); config.host = host.parse().unwrap();
            assert!(service_info(&config).is_err());
        }
    }
}
