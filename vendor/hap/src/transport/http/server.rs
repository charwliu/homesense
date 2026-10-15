use crate::transport::http::body::Body;
use futures::{
    channel::oneshot,
    future::{self, BoxFuture, Future, FutureExt, TryFutureExt},
    lock::Mutex,
};
use hyper::{
    body::Incoming, server::conn::http1, service::Service, Method, Request, Response, StatusCode,
};
use log::{error, info};
use std::{
    net::SocketAddr,
    pin::Pin,
    sync::Arc,
};
#[cfg(test)]
use std::task::{Context, Poll};
use tokio::net::TcpListener;

use crate::{
    event::Event,
    pointer,
    transport::{
        http::{
            event_response,
            handler::{
                accessories::Accessories,
                characteristics::{GetCharacteristics, UpdateCharacteristics},
                identify::Identify,
                pair_setup::PairSetup,
                pair_verify::PairVerify,
                pairings::Pairings,
                HandlerExt, JsonHandler, TlvHandler,
            },
            status_response, EventObject,
        },
        tcp::{EncryptedStream, Session, StreamWrapper},
    },
    Error, Result,
};

struct Handlers {
    pub pair_setup: Arc<Mutex<Box<dyn HandlerExt + Send + Sync>>>,
    pub pair_verify: Arc<Mutex<Box<dyn HandlerExt + Send + Sync>>>,
    pub accessories: Arc<Mutex<Box<dyn HandlerExt + Send + Sync>>>,
    pub get_characteristics: Arc<Mutex<Box<dyn HandlerExt + Send + Sync>>>,
    pub put_characteristics: Arc<Mutex<Box<dyn HandlerExt + Send + Sync>>>,
    pub pairings: Arc<Mutex<Box<dyn HandlerExt + Send + Sync>>>,
    pub identify: Arc<Mutex<Box<dyn HandlerExt + Send + Sync>>>,
}

struct Api {
    controller_id: pointer::ControllerId,
    event_subscriptions: pointer::EventSubscriptions,
    config: pointer::Config,
    storage: pointer::Storage,
    accessory_database: pointer::AccessoryDatabase,
    event_emitter: pointer::EventEmitter,
    handlers: Handlers,
}

impl Api {
    fn new(
        controller_id: pointer::ControllerId,
        event_subscriptions: pointer::EventSubscriptions,
        config: pointer::Config,
        storage: pointer::Storage,
        accessory_database: pointer::AccessoryDatabase,
        event_emitter: pointer::EventEmitter,
        session_sender: oneshot::Sender<Session>,
    ) -> Self {
        Api {
            controller_id,
            event_subscriptions,
            config,
            storage,
            accessory_database,
            event_emitter,
            handlers: Handlers {
                pair_setup: Arc::new(Mutex::new(Box::new(TlvHandler::from(PairSetup::new())))),
                pair_verify: Arc::new(Mutex::new(Box::new(TlvHandler::from(PairVerify::new(
                    session_sender,
                ))))),
                accessories: Arc::new(Mutex::new(Box::new(JsonHandler::from(Accessories::new())))),
                get_characteristics: Arc::new(Mutex::new(Box::new(JsonHandler::from(
                    GetCharacteristics::new(),
                )))),
                put_characteristics: Arc::new(Mutex::new(Box::new(JsonHandler::from(
                    UpdateCharacteristics::new(),
                )))),
                pairings: Arc::new(Mutex::new(Box::new(TlvHandler::from(Pairings::new())))),
                identify: Arc::new(Mutex::new(Box::new(JsonHandler::from(Identify::new())))),
            },
        }
    }
}

impl Service<Request<Incoming>> for Api {
    type Error = Error;
    type Future =
        Pin<Box<dyn Future<Output = std::result::Result<Self::Response, Self::Error>> + Send>>;
    type Response = Response<Body>;

    fn call(&self, req: Request<Incoming>) -> Self::Future {
        let (parts, body) = req.into_parts();
        let body = Body::from(body);
        let method = parts.method;
        let uri = parts.uri;

        let mut handler: Option<Arc<Mutex<Box<dyn HandlerExt + Send + Sync>>>> =
            match (method, uri.path()) {
                (Method::POST, "/pair-setup") => Some(self.handlers.pair_setup.clone()),
                (Method::POST, "/pair-verify") => Some(self.handlers.pair_verify.clone()),
                (Method::GET, "/accessories") => Some(self.handlers.accessories.clone()),
                (Method::GET, "/characteristics") => {
                    Some(self.handlers.get_characteristics.clone())
                }
                (Method::PUT, "/characteristics") => {
                    Some(self.handlers.put_characteristics.clone())
                }
                (Method::POST, "/pairings") => Some(self.handlers.pairings.clone()),
                (Method::POST, "/identify") => Some(self.handlers.identify.clone()),
                _ => None,
            };

        let controller_id = self.controller_id.clone();
        let event_subscriptions = self.event_subscriptions.clone();
        let config = self.config.clone();
        let storage = self.storage.clone();
        let accessory_database = self.accessory_database.clone();
        let event_emitter = self.event_emitter.clone();

        let fut = async move {
            info!(
                "HAP request path={} authenticated={}",
                uri.path(),
                controller_id.read().is_ok_and(|id| id.is_some())
            );
            let result = match handler.take() {
                Some(handler) => {
                    handler
                        .lock()
                        .await
                        .handle(
                            uri,
                            body,
                            controller_id,
                            event_subscriptions,
                            config,
                            storage,
                            accessory_database,
                            event_emitter,
                        )
                        .await
                }
                None => future::ready(status_response(StatusCode::NOT_FOUND)).await,
            };
            match &result {
                Ok(response) => info!("HAP response status={}", response.status()),
                Err(_) => error!("HAP request handler failed"),
            }
            result
        }
        .boxed();

        fut
    }
}

#[derive(Clone)]
pub struct Server {
    config: pointer::Config,
    storage: pointer::Storage,
    accessory_database: pointer::AccessoryDatabase,
    event_emitter: pointer::EventEmitter,
    mdns_responder: pointer::MdnsResponder,
}

impl Server {
    pub fn new(
        config: pointer::Config,
        storage: pointer::Storage,
        accessory_database: pointer::AccessoryDatabase,
        event_emitter: pointer::EventEmitter,
        mdns_responder: pointer::MdnsResponder,
    ) -> Self {
        Server {
            config,
            storage,
            accessory_database,
            event_emitter,
            mdns_responder,
        }
    }

    pub fn run_handle(&self) -> BoxFuture<'_, Result<()>> {
        self.run_handle_ready(None)
    }

    pub(crate) fn run_handle_ready(
        &self,
        ready: Option<tokio::sync::oneshot::Sender<()>>,
    ) -> BoxFuture<'_, Result<()>> {
        let config = self.config.clone();
        let storage = self.storage.clone();
        let accessory_database = self.accessory_database.clone();
        let event_emitter = self.event_emitter.clone();
        let mdns_responder = self.mdns_responder.clone();

        async move {
            let config_lock = config.lock().await;
            let socket_addr = SocketAddr::new(config_lock.host, config_lock.port);
            drop(config_lock);

            info!("binding TCP listener on {}", &socket_addr);
            let listener = TcpListener::bind(socket_addr).await?;

            mdns_responder.lock().await.update_records().await?;
            if let Some(ready) = ready { let _ = ready.send(()); }

            // Closing the listener future also ends every accepted session.
            // Sessions retain receivers only, so no task can prolong this scope.
            let (session_scope, _) = tokio::sync::watch::channel(());
            let slots = Arc::new(tokio::sync::Semaphore::new(32));
            loop {
                let (stream, _socket_addr) = listener.accept().await?;

                let permit = match slots.clone().try_acquire_owned() {
                    Ok(permit) => permit,
                    Err(_) => {
                        drop(stream);
                        continue;
                    }
                };
                info!("HAP TCP accepted peer={}", _socket_addr);

                let (
                    encrypted_stream,
                    stream_incoming,
                    stream_outgoing,
                    session_sender,
                    incoming_waker,
                    outgoing_waker,
                ) = EncryptedStream::new(stream);
                let stream_wrapper = StreamWrapper::new(
                    stream_incoming,
                    stream_outgoing.clone(),
                    incoming_waker,
                    outgoing_waker,
                );
                let event_subscriptions = Arc::new(Mutex::new(vec![]));

                let session_controller = encrypted_stream.controller_id.clone();
                let api = Api::new(
                    encrypted_stream.controller_id.clone(),
                    event_subscriptions.clone(),
                    config.clone(),
                    storage.clone(),
                    accessory_database.clone(),
                    event_emitter.clone(),
                    session_sender,
                );

                let notification_controller = session_controller.clone();
                let notification_storage = storage.clone();
                let listener_id = event_emitter
                    .lock()
                    .await
                    .add_listener(Box::new(move |event| {
                        let event_subscriptions_ = event_subscriptions.clone();
                        let stream_outgoing_ = stream_outgoing.clone();
                        let controller = notification_controller.clone();
                        let storage = notification_storage.clone();
                        async move {
                            match *event {
                                Event::CharacteristicValueChanged {
                                    aid,
                                    iid,
                                    ref value,
                                } => {
                                    let mut dropped_subscriptions = vec![];
                                    for (i, &(s_aid, s_iid)) in
                                        event_subscriptions_.lock().await.iter().enumerate()
                                    {
                                        if s_aid == aid && s_iid == iid {
                                            let event = EventObject {
                                                aid,
                                                iid,
                                                value: value.clone(),
                                            };
                                            let event_res = event_response(vec![event])
                                                .expect("couldn't create event response");
                                            if send_authorized_event(
                                                &controller,
                                                &storage,
                                                &stream_outgoing_,
                                                event_res,
                                            )
                                            .await
                                            .is_err()
                                            {
                                                dropped_subscriptions.push(i);
                                            }
                                        }
                                    }
                                    let mut ev = event_subscriptions_.lock().await;
                                    for s in dropped_subscriptions {
                                        ev.remove(s);
                                    }
                                }
                                _ => {}
                            }
                        }
                        .boxed()
                    }));

                let mut http = http1::Builder::new();
                http.half_close(true);
                http.keep_alive(true);
                http.preserve_header_case(true);

                let emitter = event_emitter.clone();
                let mut listener_closed = session_scope.subscribe();
                tokio::spawn(async move {
                    let transport = encrypted_stream.map_err(|e| error!("{:?}", e)).map(|_| ());
                    let http = http
                        .serve_connection(hyper_util::rt::TokioIo::new(stream_wrapper), api)
                        .map_err(|e| error!("{:?}", e))
                        .map(|_| ());
                    tokio::select! {
                        biased;
                        _ = listener_closed.changed() => {},
                        _ = supervise_session(
                            transport,
                            http,
                            session_controller,
                            std::time::Duration::from_secs(60),
                        ) => {},
                    }
                    info!("HAP TCP closed peer={}", _socket_addr);
                    emitter.lock().await.remove_listener(listener_id);
                    drop(permit);
                });
            }

            #[allow(unreachable_code)]
            Ok(())
        }
        .boxed()
    }
}

/// Completing either I/O half or expiring the unauthenticated deadline drops
/// both halves. Authenticated idle sessions remain open for event subscriptions.
async fn supervise_session<T, H>(
    transport: T,
    http: H,
    controller: pointer::ControllerId,
    auth_timeout: std::time::Duration,
) where
    T: Future<Output = ()>,
    H: Future<Output = ()>,
{
    let io = async move {
        match future::select(Box::pin(transport), Box::pin(http)).await {
            future::Either::Left(((), remaining)) => {
                info!("HAP session transport ended");
                drop(remaining);
            }
            future::Either::Right(((), remaining)) => {
                info!("HAP session HTTP ended");
                drop(remaining);
            }
        }
    };
    let authentication = async move {
        tokio::time::sleep(auth_timeout).await;
        let authenticated = controller.read().map(|id| id.is_some()).unwrap_or(false);
        if authenticated {
            future::pending::<()>().await;
        }
        info!("HAP session authentication deadline expired");
    };
    match future::select(Box::pin(io), Box::pin(authentication)).await {
        future::Either::Left(((), remaining)) => drop(remaining),
        future::Either::Right(((), remaining)) => drop(remaining),
    }
}

#[cfg(test)]
mod session_tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        RwLock,
    };
    use std::time::Duration;
    struct PendingHalf(Arc<AtomicUsize>);
    impl Future for PendingHalf {
        type Output = ();
        fn poll(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<()> {
            Poll::Pending
        }
    }
    impl Drop for PendingHalf {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }
    #[tokio::test]
    async fn unauthenticated_deadline_drops_both_halves() {
        let drops = Arc::new(AtomicUsize::new(0));
        tokio::time::timeout(
            Duration::from_secs(1),
            supervise_session(
                PendingHalf(drops.clone()),
                PendingHalf(drops.clone()),
                Arc::new(RwLock::new(None)),
                Duration::from_millis(5),
            ),
        )
        .await
        .unwrap();
        assert_eq!(drops.load(Ordering::SeqCst), 2);
    }
    #[tokio::test]
    async fn authenticated_idle_connection_survives_deadline_but_io_completion_cleans_up() {
        let drops = Arc::new(AtomicUsize::new(0));
        let (finish, receiver) = oneshot::channel::<()>();
        let count = drops.clone();
        let http = async move {
            let _guard = PendingHalf(count);
            let _ = receiver.await;
        };
        let session = supervise_session(
            PendingHalf(drops.clone()),
            http,
            Arc::new(RwLock::new(Some(uuid::Uuid::new_v4()))),
            Duration::from_millis(5),
        );
        futures::pin_mut!(session);
        assert!(
            tokio::time::timeout(Duration::from_millis(30), &mut session)
                .await
                .is_err()
        );
        assert_eq!(drops.load(Ordering::SeqCst), 0);
        finish.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(1), session)
            .await
            .unwrap();
        assert_eq!(drops.load(Ordering::SeqCst), 2);
    }
}

/// Serialize the pairing check and notification enqueue with local revocation.
async fn send_authorized_event(
    controller: &pointer::ControllerId,
    storage: &pointer::Storage,
    output: &crate::transport::tcp::OutputSender,
    bytes: Vec<u8>,
) -> Result<()> {
    let id = controller.read().ok().and_then(|id| *id);
    let storage = storage.lock().await;
    let authorized = match id {
        Some(id) => storage
            .load_pairing(&id)
            .await
            .map(|p| p.id == id)
            .unwrap_or(false),
        None => false,
    };
    if !authorized {
        output.abort();
        return Err(Error::Storage);
    }
    output.try_send(bytes)?;
    Ok(())
}

#[cfg(test)]
mod notification_authorization_tests {
    use super::*;
    use crate::{
        pairing::{Pairing, Permissions},
        storage::{FileStorage, Storage},
        transport::tcp::OutputSender,
    };
    use futures::StreamExt;
    use std::sync::RwLock;
    use uuid::Uuid;

    #[tokio::test]
    async fn slow_subscriber_overflow_does_not_block_a_healthy_session() {
        let path = std::env::temp_dir().join(format!("hap-notify-slow-{}", Uuid::new_v4()));
        let mut store = FileStorage::new(path.as_os_str()).await.unwrap();
        let pairing = Pairing::new(Uuid::new_v4(), Permissions::Admin, [3; 32]);
        store.save_pairing(&pairing).await.unwrap();
        let storage: pointer::Storage = Arc::new(Mutex::new(Box::new(store)));
        // Two sessions of the same authorized controller have independent output budgets.
        let controller = Arc::new(RwLock::new(Some(pairing.id)));
        let (slow, slow_receiver) = OutputSender::channel();
        let (healthy, mut healthy_receiver) = OutputSender::channel();
        let mut overflowed = false;
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            for value in 0..128 {
                let bytes = event_response(vec![EventObject {
                    aid: 2,
                    iid: 9,
                    value: serde_json::json!(value),
                }])
                .unwrap();
                if send_authorized_event(&controller, &storage, &slow, bytes.clone())
                    .await
                    .is_err()
                {
                    overflowed = true;
                }
                send_authorized_event(&controller, &storage, &healthy, bytes.clone())
                    .await
                    .unwrap();
                assert_eq!(&*healthy_receiver.next().await.unwrap(), bytes.as_slice());
            }
        })
        .await
        .expect("notification path waited for the slow consumer");
        assert!(overflowed, "unread output must hit its bounded capacity");
        assert!(
            slow.try_send(vec![0]).is_err(),
            "overflow must terminate that session"
        );
        // Output congestion must neither revoke the controller nor poison another session.
        assert!(storage.lock().await.load_pairing(&pairing.id).await.is_ok());
        send_authorized_event(&controller, &storage, &healthy, vec![7])
            .await
            .unwrap();
        assert_eq!(&*healthy_receiver.next().await.unwrap(), &[7]);
        drop(slow_receiver);
        drop(storage);
        std::fs::remove_dir_all(path).unwrap();
    }

    #[tokio::test]
    async fn revoked_subscriber_cannot_receive_another_event() {
        let path = std::env::temp_dir().join(format!("hap-notify-auth-{}", Uuid::new_v4()));
        let mut store = FileStorage::new(path.as_os_str()).await.unwrap();
        let revoked = Pairing::new(Uuid::new_v4(), Permissions::User, [1; 32]);
        let retained = Pairing::new(Uuid::new_v4(), Permissions::Admin, [2; 32]);
        store.save_pairing(&revoked).await.unwrap();
        store.save_pairing(&retained).await.unwrap();
        let storage: pointer::Storage = Arc::new(Mutex::new(Box::new(store)));
        let controller = Arc::new(RwLock::new(Some(revoked.id)));
        let other = Arc::new(RwLock::new(Some(retained.id)));
        let (output, mut receiver) = OutputSender::channel();
        let (other_output, mut other_receiver) = OutputSender::channel();
        assert!(
            send_authorized_event(&controller, &storage, &output, vec![1])
                .await
                .is_ok()
        );
        assert_eq!(&*receiver.next().await.unwrap(), &[1]);
        storage
            .lock()
            .await
            .delete_pairing(&revoked.id)
            .await
            .unwrap();
        assert!(
            send_authorized_event(&controller, &storage, &output, vec![2])
                .await
                .is_err()
        );
        assert!(output.try_send(vec![3]).is_err());
        assert!(
            send_authorized_event(&other, &storage, &other_output, vec![4])
                .await
                .is_ok()
        );
        assert_eq!(&*other_receiver.next().await.unwrap(), &[4]);
        drop(storage);
        std::fs::remove_dir_all(path).unwrap();
    }
}
