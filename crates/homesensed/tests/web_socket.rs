use futures_util::{SinkExt, StreamExt};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_tungstenite::tungstenite::{Message, client::IntoClientRequest};
use uuid::Uuid;
async fn http(address: std::net::SocketAddr, method: &str, path: &str, headers: &str) -> String {
    let mut socket = tokio::net::TcpStream::connect(address).await.unwrap();
    socket.write_all(format!("{method} {path} HTTP/1.1\r\nHost: {address}\r\nOrigin: http://{address}\r\nConnection: close\r\nContent-Length: 0\r\n{headers}\r\n").as_bytes()).await.unwrap();
    let mut bytes = vec![];
    tokio::time::timeout(Duration::from_secs(3), socket.read_to_end(&mut bytes))
        .await
        .unwrap()
        .unwrap();
    String::from_utf8(bytes).unwrap()
}
async fn connect(
    address: std::net::SocketAddr,
    cookie: &str,
    origin: &str,
) -> Result<
    tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
    tokio_tungstenite::tungstenite::Error,
> {
    let mut request = format!("ws://{address}/api/v1/events")
        .into_client_request()
        .unwrap();
    request
        .headers_mut()
        .insert("origin", origin.parse().unwrap());
    request
        .headers_mut()
        .insert("cookie", cookie.parse().unwrap());
    let stream = tokio::net::TcpStream::connect(address).await.unwrap();
    tokio_tungstenite::client_async(request, stream)
        .await
        .map(|(stream, _)| stream)
}
async fn frame(
    socket: &mut tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
) -> serde_json::Value {
    let Some(Ok(Message::Text(text))) = tokio::time::timeout(Duration::from_secs(3), socket.next())
        .await
        .unwrap()
    else {
        panic!("expected state frame")
    };
    serde_json::from_str(&text).unwrap()
}
#[tokio::test]
async fn websocket_cookie_resume_origin_and_logout() {
    let root = std::env::temp_dir().join(format!("homesense-ws-{}", Uuid::new_v4()));
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let secret = "abcdefghijklmnopqrstuvwxyz0123456789";
    let directory = root.clone();
    let service = tokio::spawn(async move {
        homesensed::service::serve_web_simulation(
            include_bytes!("../../../config/simulation.json"),
            br#"{"schema_version":1,"configuration_revision":1,"variables":[],"rules":[]}"#,
            &directory,
            Duration::from_secs(8),
            homesensed::service::WebSimulation {
                address,
                secret: secret.into(),
                administrator: false,
                tls:None,
            },
        )
        .await
    });
    for _ in 0..200 {
        if tokio::net::TcpStream::connect(address).await.is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let response = http(
        address,
        "POST",
        "/api/v1/session",
        &format!("Authorization: Bearer {secret}\r\n"),
    )
    .await;
    assert!(response.starts_with("HTTP/1.1 200"));
    let cookie = response
        .lines()
        .find_map(|line| line.strip_prefix("set-cookie: "))
        .unwrap()
        .split(';')
        .next()
        .unwrap();
    let body: serde_json::Value =
        serde_json::from_str(response.split_once("\r\n\r\n").unwrap().1).unwrap();
    assert!(
        connect(address, cookie, "http://evil.invalid")
            .await
            .is_err()
    );
    assert!(
        connect(
            address,
            "homesense_session=invalid",
            &format!("http://{address}")
        )
        .await
        .is_err()
    );
    let mut socket = connect(address, cookie, &format!("http://{address}"))
        .await
        .unwrap();
    socket
        .send(Message::Text(r#"{"cursor":null}"#.into()))
        .await
        .unwrap();
    let initial = frame(&mut socket).await;
    assert_eq!(initial["reset"], true);
    assert_eq!(initial["devices"].as_array().unwrap().len(), 1);
    socket.close(None).await.unwrap();
    drop(socket);
    let mut resumed = connect(address, cookie, &format!("http://{address}"))
        .await
        .unwrap();
    resumed
        .send(Message::Text(
            serde_json::json!({"cursor":initial["cursor"]})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    let next = frame(&mut resumed).await;
    assert_eq!(next["reset"], false);
    let response = http(
        address,
        "DELETE",
        "/api/v1/session",
        &format!(
            "Cookie: {cookie}\r\nX-CSRF-Token: {}\r\n",
            body["csrf"].as_str().unwrap()
        ),
    )
    .await;
    assert!(response.starts_with("HTTP/1.1 200"));
    let close = tokio::time::timeout(Duration::from_secs(3), resumed.next())
        .await
        .unwrap();
    assert!(matches!(close, Some(Ok(Message::Close(_))) | None));
    service.await.unwrap().unwrap();
    std::fs::remove_dir_all(root).unwrap();
}
