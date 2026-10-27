use futures_util::{SinkExt, StreamExt};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_tungstenite::tungstenite::{Message, client::IntoClientRequest};
use uuid::Uuid;
async fn http(address: std::net::SocketAddr, method: &str, path: &str, headers: &str) -> String {
    http_body(address, method, path, headers, "").await
}
async fn http_body(
    address: std::net::SocketAddr,
    method: &str,
    path: &str,
    headers: &str,
    body: &str,
) -> String {
    let mut socket = tokio::net::TcpStream::connect(address).await.unwrap();
    // Each existing fixture call represents a distinct management intent.
    let idempotency = if matches!(method, "POST" | "PATCH") && path.starts_with("/api/v1/credentials") {
        format!("Idempotency-Key: {}\r\n", Uuid::new_v4())
    } else { String::new() };
    socket.write_all(format!("{method} {path} HTTP/1.1\r\nHost: {address}\r\nOrigin: http://{address}\r\nConnection: close\r\n{idempotency}Content-Type: application/json\r\nContent-Length: {}\r\n{headers}\r\n{body}",body.len()).as_bytes()).await.unwrap();
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
                tls: None,
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
        .send(Message::Text(r#"{"cursor":null,"modules":true}"#.into()))
        .await
        .unwrap();
    let initial = frame(&mut socket).await;
    assert!(initial["module_inventory"].is_null(), "member must not receive hardware inventory");
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

#[tokio::test]
async fn changed_grants_close_existing_sockets_and_resume_with_reduced_scope() {
    let root = std::env::temp_dir().join(format!("homesense-ws-grant-{}", Uuid::new_v4()));
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let directory = root.clone();
    let secret = "abcdefghijklmnopqrstuvwxyz0123456789";
    let service = tokio::spawn(async move {
        homesensed::service::serve_web_simulation(
            include_bytes!("../../../config/simulation.json"),
            br#"{"schema_version":1,"configuration_revision":1,"variables":[],"rules":[]}"#,
            &directory,
            Duration::from_secs(8),
            homesensed::service::WebSimulation {
                address,
                secret: secret.into(),
                administrator: true,
                tls: None,
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
    let auth = format!("Authorization: Bearer {secret}\r\n");
    let origin = format!("http://{address}");
    let issued = http_body(
        address,
        "POST",
        "/api/v1/credentials",
        &auth,
        r#"{"role":"Viewer","devices":["simulation:light"],"scenes":[]}"#,
    )
    .await;
    assert!(issued.starts_with("HTTP/1.1 201"));
    let issued: serde_json::Value =
        serde_json::from_str(issued.split_once("\r\n\r\n").unwrap().1).unwrap();
    let user_auth = format!(
        "Authorization: Bearer {}\r\n",
        issued["secret"].as_str().unwrap()
    );
    let login = http(address, "POST", "/api/v1/session", &user_auth).await;
    let cookie = login
        .lines()
        .find_map(|line| line.strip_prefix("set-cookie: "))
        .unwrap()
        .split(';')
        .next()
        .unwrap();
    let mut socket = connect(address, cookie, &origin).await.unwrap();
    socket
        .send(Message::Text(r#"{"cursor":null}"#.into()))
        .await
        .unwrap();
    let initial = frame(&mut socket).await;
    assert_eq!(initial["devices"].as_array().unwrap().len(), 1);
    let update =
        serde_json::json!({"expected":issued["grant"],"role":"Viewer","devices":[],"scenes":[]})
            .to_string();
    let changed = http_body(
        address,
        "PATCH",
        &format!("/api/v1/credentials/{}", issued["id"].as_str().unwrap()),
        &auth,
        &update,
    )
    .await;
    assert!(changed.starts_with("HTTP/1.1 200"));
    let closed = tokio::time::timeout(Duration::from_secs(3), socket.next())
        .await
        .unwrap();
    assert!(matches!(closed, Some(Ok(Message::Close(_))) | None));
    assert!(connect(address, cookie, &origin).await.is_err());
    let login = http(address, "POST", "/api/v1/session", &user_auth).await;
    let cookie = login
        .lines()
        .find_map(|line| line.strip_prefix("set-cookie: "))
        .unwrap()
        .split(';')
        .next()
        .unwrap();
    let mut resumed = connect(address, cookie, &origin).await.unwrap();
    resumed
        .send(Message::Text(
            serde_json::json!({"cursor":initial["cursor"]})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    let state = frame(&mut resumed).await;
    assert_eq!(state["reset"], true);
    assert!(state["devices"].as_array().unwrap().is_empty());
    let revoked = http(
        address,
        "POST",
        &format!(
            "/api/v1/credentials/{}/revoke",
            issued["id"].as_str().unwrap()
        ),
        &auth,
    )
    .await;
    assert!(revoked.starts_with("HTTP/1.1 200"));
    let closed = tokio::time::timeout(Duration::from_secs(3), resumed.next())
        .await
        .unwrap();
    assert!(matches!(closed, Some(Ok(Message::Close(_))) | None));
    assert!(connect(address, cookie, &origin).await.is_err());
    service.await.unwrap().unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn empty_room_directory_is_identical_over_http_wss_and_reconnect_without_expanding_scope() {
    let root=std::env::temp_dir().join(format!("rooms-ws-{}",Uuid::new_v4()));
    let listener=std::net::TcpListener::bind("127.0.0.1:0").unwrap();let address=listener.local_addr().unwrap();drop(listener);
    let mut config:serde_json::Value=serde_json::from_slice(include_bytes!("../../../config/simulation.json")).unwrap();
    config["floors"]=serde_json::json!(["Floor 1","Floor 2","Floor 3","Unused floor"]);
    config["rooms"]=serde_json::json!([{"id":"visible","name":"Visible room","floor":"Floor 1"},{"id":"empty","name":"Empty room","floor":"Floor 2"},{"id":"hidden","name":"Hidden room","floor":"Floor 3"}]);
    config["devices"][0]["room"]=serde_json::json!("visible");
    config["devices"].as_array_mut().unwrap().push(serde_json::json!({"id":"hidden:raw","name":"Hidden raw","gateway":"simulation","address":"15","channel":2,"enabled":true,"catalog_visible":false,"room":"hidden"}));
    let directory=root.clone();let secret="abcdefghijklmnopqrstuvwxyz0123456789";
    let service=tokio::spawn(async move{homesensed::service::serve_web_simulation(&serde_json::to_vec(&config).unwrap(),br#"{"schema_version":1,"configuration_revision":1,"variables":[],"rules":[]}"#,&directory,Duration::from_secs(6),homesensed::service::WebSimulation{address,secret:secret.into(),administrator:true,tls:None}).await});
    for _ in 0..200 {if tokio::net::TcpStream::connect(address).await.is_ok(){break;}tokio::time::sleep(Duration::from_millis(10)).await;}
    fn body(r:&str)->serde_json::Value {assert!(r.starts_with("HTTP/1.1 200")||r.starts_with("HTTP/1.1 201"),"{r}");serde_json::from_str(r.split_once("\r\n\r\n").unwrap().1).unwrap()}
    fn cookie(r:&str)->String {r.lines().find_map(|line|line.strip_prefix("set-cookie: ")).unwrap().split(';').next().unwrap().to_owned()}
    let auth=format!("Authorization: Bearer {secret}\r\n");let origin=format!("http://{address}");
    let admin_rooms=body(&http(address,"GET","/api/v1/rooms",&auth).await);assert_eq!(admin_rooms["rooms"].as_array().unwrap().len(),3);assert_eq!(admin_rooms["floors"].as_array().unwrap().len(),4);
    // Narrow configuration administrator can read all metadata but only one endpoint.
    let issued=body(&http_body(address,"POST","/api/v1/credentials",&auth,r#"{"role":"Administrator","devices":["simulation:light"],"scenes":[]}"#).await);
    let narrow=issued["secret"].as_str().unwrap();let narrow_auth=format!("Authorization: Bearer {narrow}\r\n");
    let login=http(address,"POST","/api/v1/session",&narrow_auth).await;let admin_cookie=cookie(&login);
    let mut admin=connect(address,&admin_cookie,&origin).await.unwrap();admin.send(Message::Text(r#"{"cursor":null}"#.into())).await.unwrap();let initial=frame(&mut admin).await;
    assert_eq!(initial["rooms"],admin_rooms["rooms"]);assert_eq!(initial["floors"],admin_rooms["floors"]);assert_eq!(initial["devices"].as_array().unwrap().len(),1);assert!(!initial.to_string().contains("hidden:raw"));
    let admin_http=body(&http(address,"GET","/api/v1/devices",&narrow_auth).await);assert_eq!(admin_http["rooms"],initial["rooms"]);assert_eq!(admin_http["devices"].as_array().unwrap().len(),1);
    // Non-managers retain scope privacy, including unused/hidden floor labels.
    let issued=body(&http_body(address,"POST","/api/v1/credentials",&auth,r#"{"role":"Member","devices":["simulation:light"],"scenes":[]}"#).await);
    let member_auth=format!("Authorization: Bearer {}\r\n",issued["secret"].as_str().unwrap());let member_rooms=body(&http(address,"GET","/api/v1/rooms",&member_auth).await);
    assert_eq!(member_rooms["rooms"].as_array().unwrap().len(),1);assert_eq!(member_rooms["floors"],serde_json::json!(["Floor 1"]));
    let login=http(address,"POST","/api/v1/session",&member_auth).await;let member_cookie=cookie(&login);let mut member=connect(address,&member_cookie,&origin).await.unwrap();member.send(Message::Text(r#"{"cursor":null}"#.into())).await.unwrap();let member_frame=frame(&mut member).await;
    assert_eq!(member_frame["rooms"],member_rooms["rooms"]);assert_eq!(member_frame["floors"],member_rooms["floors"]);assert_eq!(member_frame["devices"].as_array().unwrap().len(),1);assert!(!member_frame.to_string().contains("hidden:raw"));assert!(!member_frame.to_string().contains("Empty room"));
    admin.close(None).await.unwrap();member.close(None).await.unwrap();
    let mut resumed=connect(address,&admin_cookie,&origin).await.unwrap();resumed.send(Message::Text(serde_json::json!({"cursor":initial["cursor"]}).to_string().into())).await.unwrap();let next=frame(&mut resumed).await;assert_eq!(next["rooms"],initial["rooms"]);assert_eq!(next["floors"],initial["floors"]);assert!(next["devices"].as_array().unwrap().iter().all(|d|d["id"]=="simulation:light"));
    resumed.close(None).await.unwrap();
    let mut resumed=connect(address,&member_cookie,&origin).await.unwrap();resumed.send(Message::Text(serde_json::json!({"cursor":member_frame["cursor"]}).to_string().into())).await.unwrap();let next=frame(&mut resumed).await;
    assert_eq!(next["rooms"],member_rooms["rooms"]);assert_eq!(next["floors"],member_rooms["floors"]);assert!(!next.to_string().contains("hidden:raw"));assert!(!next.to_string().contains("Empty room"));
    resumed.close(None).await.unwrap();service.await.unwrap().unwrap();std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn administrator_receives_complete_module_state_on_the_existing_socket() {
    let root=std::env::temp_dir().join(format!("homesense-ws-modules-{}",Uuid::new_v4()));
    let listener=std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address=listener.local_addr().unwrap();drop(listener);
    let secret="module-push-admin-synthetic-01234567890123456789";
    let directory=root.clone();
    let service=tokio::spawn(async move {
        homesensed::service::serve_web_simulation(include_bytes!("../../../config/simulation.json"),
            br#"{"schema_version":1,"configuration_revision":1,"variables":[],"rules":[]}"#,
            &directory,Duration::from_secs(5),homesensed::service::WebSimulation {address,secret:secret.into(),administrator:true,tls:None}).await
    });
    for _ in 0..200 {if tokio::net::TcpStream::connect(address).await.is_ok(){break;}tokio::time::sleep(Duration::from_millis(10)).await;}
    let login=http(address,"POST","/api/v1/session",&format!("Authorization: Bearer {secret}\r\n")).await;
    let cookie=login.lines().find_map(|l|l.strip_prefix("set-cookie: ")).unwrap().split(';').next().unwrap();
    let mut socket=connect(address,cookie,&format!("http://{address}")).await.unwrap();
    socket.send(Message::Text(r#"{"cursor":null,"modules":true}"#.into())).await.unwrap();
    let initial=frame(&mut socket).await;
    assert_eq!(initial["type"],"state");
    assert_eq!(initial["module_inventory"]["current"],true,"{initial}");
    let pages=initial["module_inventory"]["pages"].as_array().unwrap();assert_eq!(pages.len(),1);
    assert_eq!(pages[0]["revision"],initial["revision"]);assert!(pages[0]["next_offset"].is_null());
    assert_eq!(pages[0]["modules"].as_array().unwrap().len(),1);assert!(pages[0]["commands"].as_array().unwrap().is_empty());
    socket.close(None).await.unwrap();
    service.await.unwrap().unwrap();std::fs::remove_dir_all(root).unwrap();
}
