//! Listener replacement must not leave old authenticated sessions alive.
use hap::{
    pairing::{Pairing, Permissions},
    server::{IpServer, Server},
    storage::Storage,
};
use homesense_hap::storage::IdentityStore;
use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    time::{Duration, Instant},
};
use uuid::Uuid;
mod support;

#[tokio::test]
async fn stopping_listener_closes_authenticated_and_unpaired_connections() {
    for paired in [false, true] {
        let root = std::env::temp_dir().join(format!("hap-session-stop-{}", Uuid::new_v4()));
        let reservation = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = reservation.local_addr().unwrap();
        let mut store = IdentityStore::open(&root).await.unwrap();
        let config = hap::Config {
            host: address.ip(),
            port: address.port(),
            ..Default::default()
        };
        let accessory_id = config.device_id.to_string();
        let key = config.device_ed25519_keypair.verifying_key();
        store.save_config(&config).await.unwrap();
        store.initialize_aid_cache().await.unwrap();
        let controller = Uuid::new_v4();
        let signing = ed25519_dalek::SigningKey::from_bytes(&[19; 32]);
        if paired {
            store
                .save_pairing(&Pairing::new(
                    controller,
                    Permissions::Admin,
                    signing.verifying_key().to_bytes(),
                ))
                .await
                .unwrap();
        }
        let server = IpServer::new_loopback(config, store.clone()).await.unwrap();
        drop(reservation);
        let running = server.clone();
        let task = tokio::spawn(async move { running.run_handle().await });
        let (ready, started) = tokio::sync::oneshot::channel();
        let (release, closed) = std::sync::mpsc::channel();
        let client = tokio::task::spawn_blocking(move || {
            let deadline = Instant::now() + Duration::from_secs(3);
            let mut stream = loop {
                match TcpStream::connect(address) {
                    Ok(s) => break s,
                    Err(e) => {
                        assert!(Instant::now() < deadline, "{e}");
                        std::thread::sleep(Duration::from_millis(10));
                    }
                }
            };
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            stream
                .set_write_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            if paired {
                let mut session =
                    support::verify(&mut stream, controller, &signing, &accessory_id, &key);
                let (headers, _) = session.request(
                    &mut stream,
                    b"GET /accessories HTTP/1.1\r\nHost: localhost\r\n\r\n",
                );
                assert!(headers.starts_with("HTTP/1.1 200"));
            } else {
                stream
                    .write_all(b"GET /accessories HTTP/1.1\r\nHost: localhost\r\n\r\n")
                    .unwrap();
                let mut header = Vec::new();
                while !header.ends_with(b"\r\n\r\n") {
                    let mut b = [0];
                    stream.read_exact(&mut b).unwrap();
                    header.push(b[0]);
                    assert!(header.len() < 8192);
                }
                let header = String::from_utf8(header).unwrap();
                let length = header
                    .lines()
                    .find_map(|line| {
                        line.split_once(':')
                            .filter(|(k, _)| k.eq_ignore_ascii_case("content-length"))
                            .map(|(_, v)| v.trim().parse::<usize>().unwrap())
                    })
                    .unwrap();
                let mut body = vec![0; length];
                stream.read_exact(&mut body).unwrap();
                assert!(header.starts_with("HTTP/1.1 401"), "{header}");
            }
            ready.send(()).unwrap();
            closed.recv_timeout(Duration::from_secs(3)).unwrap();
            let mut byte = [0];
            match stream.read(&mut byte) {
                Ok(0) => {}
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::ConnectionAborted
                    ) => {}
                result => panic!("old session survived listener stop: {result:?}"),
            }
        });
        tokio::time::timeout(Duration::from_secs(4), started)
            .await
            .unwrap()
            .unwrap();
        task.abort();
        let _ = task.await;
        release.send(()).unwrap();
        client.await.unwrap();
        let rebound = TcpListener::bind(address).unwrap();
        drop(rebound);
        assert_eq!(store.count_pairings().await.unwrap(), usize::from(paired));
        drop(server);
        drop(store);
        std::fs::remove_dir_all(root).unwrap();
    }
}
