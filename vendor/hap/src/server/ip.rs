use async_trait::async_trait;
use futures::{
    future::{BoxFuture, FutureExt},
    lock::Mutex,
};
use log::{error, info};
use std::sync::Arc;

use crate::{
    accessory::HapAccessory,
    config::Config,
    event::{Event, EventEmitter},
    pointer,
    server::Server,
    storage::{accessory_database::AccessoryDatabase, Storage},
    transport::{http::server::Server as HttpServer, mdns::MdnsResponder},
    BonjourStatusFlag, Result,
};

/// HAP Server via TCP/IP.
#[derive(Clone)]
pub struct IpServer {
    config: pointer::Config,
    storage: pointer::Storage,
    accessory_database: pointer::AccessoryDatabase,
    http_server: HttpServer,
    mdns_responder: pointer::MdnsResponder,
    aid_cache: Arc<Mutex<Vec<u64>>>,
}

impl IpServer {
    /// Install before serving requests. All characteristic writes then use this
    /// grouped adapter, without falling back to individual value callbacks.
    pub async fn set_write_batch_handler(&self, handler: Arc<dyn super::WriteBatchHandler>) {
        self.accessory_database
            .lock()
            .await
            .set_write_batch_handler(handler);
    }

    /// Run the complete service, acknowledging only after TCP bind and initial
    /// discovery records succeed. A dropped sender is a startup failure, never
    /// readiness. The caller must still supervise the returned future.
    pub fn run_handle_ready(
        &self,
        ready: tokio::sync::oneshot::Sender<()>,
    ) -> BoxFuture<Result<()>> {
        let http = self.http_server.run_handle_ready(Some(ready));
        let responder = self.mdns_responder.clone();
        async move {
            let discovery = async move {
                let handle = responder.lock().await.run_handle();
                handle.await
            };
            futures::try_join!(http, discovery)?;
            Ok(())
        }.boxed()
    }

    /// Register an existing shared graph during startup. On any failure discard
    /// this server instance; live graph publication needs a separate transaction.
    pub async fn add_accessory_handle(&self, accessory: pointer::Accessory) -> Result<()> {
        let aid = accessory.lock().await.get_id();
        self.accessory_database
            .lock()
            .await
            .add_accessory_handle(accessory)
            .await?;
        let mut aid_cache = self.aid_cache.lock().await;
        if !aid_cache.contains(&aid) {
            let mut next_aids = aid_cache.clone();
            next_aids.push(aid);
            let mut config = self.config.lock().await;
            let mut next: Config = serde_json::from_slice(&serde_json::to_vec(&*config)?)?;
            next.configuration_number = config
                .configuration_number
                .checked_add(1)
                .ok_or(crate::Error::Storage)?;
            self.storage
                .lock()
                .await
                .save_graph_state(config.configuration_number, &next, &next_aids)
                .await?;
            *config = next;
            *aid_cache = next_aids;
        }
        Ok(())
    }

    /// Install a complete graph into an empty, not-yet-serving server.
    /// Persist the complete AID set once; publish handles only after commit.
    pub async fn install_startup_graph(
        &self,
        accessories: Vec<pointer::Accessory>,
        handler: Arc<dyn super::WriteBatchHandler>,
    ) -> Result<()> {
        let aids = AccessoryDatabase::startup_aids(&accessories).await?;
        let mut db = self.accessory_database.lock().await;
        if !db.accessories.is_empty() {
            return Err(crate::Error::DuplicateAccessory);
        }
        let mut guards = Vec::new();
        for accessory in &accessories {
            guards.push(accessory.lock().await);
        }
        // Recheck IDs while holding all graph locks across persistence.
        let mut locked_aids: Vec<_> = guards.iter().map(|a| a.get_id()).collect();
        locked_aids.sort_unstable();
        if locked_aids != aids {
            return Err(crate::Error::Storage);
        }
        let manifest = AccessoryDatabase::graph_manifest(
            guards
                .iter()
                .map(|a| serde_json::to_value(&**a))
                .collect::<std::result::Result<Vec<_>, _>>()?,
        )?;
        let mut cache = self.aid_cache.lock().await;
        let mut old = cache.clone();
        old.sort_unstable();
        let prior = self.storage.lock().await.load_graph_manifest().await?;
        if old != aids || prior.as_deref() != Some(manifest.as_slice()) {
            let mut config = self.config.lock().await;
            let mut next: Config = serde_json::from_slice(&serde_json::to_vec(&*config)?)?;
            next.configuration_number = config
                .configuration_number
                .checked_add(1)
                .ok_or(crate::Error::Storage)?;
            self.storage
                .lock()
                .await
                .save_graph_manifest(config.configuration_number, &next, &aids, &manifest)
                .await?;
            *config = next;
            *cache = aids;
        }
        for accessory in &mut guards {
            accessory.set_event_emitter_on_characteristics(Some(db.event_emitter()));
        }
        drop(guards);
        db.accessories = accessories;
        db.set_write_batch_handler(handler);
        Ok(())
    }

    /// Creates a new [`IpServer`](IpServer).
    ///
    /// # Examples
    /// ```no_run
    /// use tokio;
    ///
    /// use hap::{
    ///     accessory::{lightbulb::LightbulbAccessory, AccessoryCategory, AccessoryInformation},
    ///     server::{IpServer, Server},
    ///     storage::{FileStorage, Storage},
    ///     Config,
    ///     MacAddress,
    ///     Pin,
    ///     Result,
    /// };
    ///
    /// #[tokio::main]
    /// async fn main() -> Result<()> {
    ///     let lightbulb = LightbulbAccessory::new(1, AccessoryInformation {
    ///         name: "Acme Lightbulb".into(),
    ///         ..Default::default()
    ///     })?;
    ///
    ///     let mut storage = FileStorage::current_dir().await?;
    ///
    ///     let config = match storage.load_config().await {
    ///         Ok(mut config) => {
    ///             config.redetermine_local_ip();
    ///             storage.save_config(&config).await?;
    ///             config
    ///         },
    ///         Err(_) => {
    ///             let config = Config {
    ///                 pin: Pin::new([1, 1, 1, 2, 2, 3, 3, 3])?,
    ///                 name: "Acme Lightbulb".into(),
    ///                 device_id: MacAddress::from([10, 20, 30, 40, 50, 60]),
    ///                 category: AccessoryCategory::Lightbulb,
    ///                 ..Default::default()
    ///             };
    ///             storage.save_config(&config).await?;
    ///             config
    ///         },
    ///     };
    ///
    ///     let mut server = IpServer::new(config, storage).await?;
    ///     server.add_accessory(lightbulb).await?;
    ///
    ///     let handle = server.run_handle();
    ///
    ///     std::env::set_var("RUST_LOG", "hap=info");
    ///     env_logger::init();
    ///
    ///     handle.await
    /// }
    /// ```
    pub async fn new<S: Storage + Send + Sync + 'static>(
        config: Config,
        storage: S,
    ) -> Result<Self> {
        Self::new_with_advertising(config, storage, true).await
    }
    /// Local integration tests only: no mDNS sockets or advertisements.
    pub async fn new_loopback<S: Storage + Send + Sync + 'static>(
        config: Config,
        storage: S,
    ) -> Result<Self> {
        if !config.host.is_loopback() {
            return Err(crate::Error::Storage);
        }
        Self::new_with_advertising(config, storage, false).await
    }
    async fn new_with_advertising<S: Storage + Send + Sync + 'static>(
        config: Config,
        storage: S,
        advertise: bool,
    ) -> Result<Self> {
        let config = Arc::new(Mutex::new(config));
        let storage: pointer::Storage = Arc::new(Mutex::new(Box::new(storage)));

        let config_ = config.clone();
        let storage_ = storage.clone();
        let mut event_emitter = EventEmitter::new();

        let mut s = storage_.lock().await;
        if s.count_pairings().await? > 0 {
            info!("1 or more controllers paired; setting Bonjour status flag to `Zero`");

            let mut c = config_.lock().await;
            c.status_flag = BonjourStatusFlag::Zero;

            s.save_config(&c).await?;
        } else {
            info!("0 controllers paired; setting Bonjour status flag to `Not Paired`");

            let mut c = config_.lock().await;
            c.status_flag = BonjourStatusFlag::NotPaired;

            s.save_config(&c).await?;
        }
        drop(s);

        let mdns_responder = Arc::new(Mutex::new(if advertise {
            MdnsResponder::new(config.clone()).await?
        } else {
            MdnsResponder::disabled(config.clone())
        }));
        let mdns_responder_ = mdns_responder.clone();

        event_emitter.add_listener(Box::new(move |event| {
            let config_ = config_.clone();
            let storage_ = storage_.clone();
            let mdns_responder_ = mdns_responder_.clone();
            async move {
                match *event {
                    Event::ControllerPaired { id } => {
                        info!("controller {} paired", id);

                        let pairing_count = storage_.lock().await.count_pairings().await;
                        if let Ok(count) = pairing_count {
                            if count > 0 {
                                info!("1 or more controllers paired; setting Bonjour status flag to `Zero`");

                                let mut c = config_.lock().await;
                                c.status_flag = BonjourStatusFlag::Zero;

                                storage_
                                    .lock()
                                    .await
                                    .save_config(&c)
                                    .await
                                    .map_err(|e| error!("error saving the config: {:?}", e))
                                    .ok();

                                drop(c);

                                if let Err(error) = mdns_responder_.lock().await.update_records().await { log::error!("mDNS update: {error}"); }
                            }
                        }
                    },
                    Event::ControllerUnpaired { id } => {
                        info!("controller {} unpaired", id);

                        let pairing_count = storage_.lock().await.count_pairings().await;
                        // TODO - `pairing_count` is an Err
                        if let Ok(count) = pairing_count {
                            if count == 0 {
                                info!("0 controllers paired; setting Bonjour status flag to `Not Paired`");

                                let mut c = config_.lock().await;
                                c.status_flag = BonjourStatusFlag::NotPaired;

                                storage_
                                    .lock()
                                    .await
                                    .save_config(&c)
                                    .await
                                    .map_err(|e| error!("error saving the config: {:?}", e))
                                    .ok();

                                drop(c);

                                if let Err(error) = mdns_responder_.lock().await.update_records().await { log::error!("mDNS update: {error}"); }
                            }
                        }
                    },
                    _ => {},
                }
            }
            .boxed()
        }));

        let event_emitter = Arc::new(Mutex::new(event_emitter));
        let accessory_database =
            Arc::new(Mutex::new(AccessoryDatabase::new(event_emitter.clone())));

        let http_server = HttpServer::new(
            config.clone(),
            storage.clone(),
            accessory_database.clone(),
            event_emitter,
            mdns_responder.clone(),
        );

        let mut storage_lock = storage.lock().await;
        let aid_cache = Arc::new(Mutex::new(storage_lock.initialize_aid_cache().await?));
        drop(storage_lock);

        let server = IpServer {
            config,
            storage,
            accessory_database,
            http_server,
            mdns_responder,
            aid_cache,
        };

        Ok(server)
    }
}

#[async_trait]
impl Server for IpServer {
    fn run_handle(&self) -> BoxFuture<Result<()>> {
        let http_handle = self.http_server.run_handle();
        let mdns_responder = self.mdns_responder.clone();

        let handle = async move {
            let mdns_handle = mdns_responder.lock().await.run_handle();

            futures::try_join!(http_handle, mdns_handle)?;

            Ok(())
        }
        .boxed();

        Box::pin(handle)
    }

    fn config_pointer(&self) -> pointer::Config {
        self.config.clone()
    }

    fn storage_pointer(&self) -> pointer::Storage {
        self.storage.clone()
    }

    async fn add_accessory<A: HapAccessory + 'static>(
        &self,
        accessory: A,
    ) -> Result<pointer::Accessory> {
        let aid = accessory.get_id();

        let accessory = self
            .accessory_database
            .lock()
            .await
            .add_accessory(Box::new(accessory))?;

        let mut aid_cache = self.aid_cache.lock().await;
        if !aid_cache.contains(&aid) {
            aid_cache.push(aid);
            self.storage.lock().await.save_aid_cache(&aid_cache).await?;

            let mut config = self.config.lock().await;
            config.configuration_number += 1;
            self.storage.lock().await.save_config(&config).await?;
        }

        Ok(accessory)
    }

    async fn remove_accessory(&self, accessory: &pointer::Accessory) -> Result<()> {
        let aid = accessory.lock().await.get_id();

        self.accessory_database
            .lock()
            .await
            .remove_accessory(&accessory)
            .await?;

        let mut aid_cache = self.aid_cache.lock().await;
        if aid_cache.contains(&aid) {
            aid_cache.retain(|id| *id != aid);
            self.storage.lock().await.save_aid_cache(&aid_cache).await?;

            let mut config = self.config.lock().await;
            config.configuration_number += 1;
        }

        Ok(())
    }

    // async fn factory_reset(&mut self) -> Result<()> {
    //     unimplemented!();

    //     Ok(())
    // }
}
