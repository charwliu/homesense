// Test-only controller for the isolated ngr simulation. Never use production state.
#[path = "../../homesense-hap/tests/support/mod.rs"]
mod support;
use hap::{storage::Storage, pairing::{Pairing, Permissions}};
use homesense_hap::storage::IdentityStore;
use std::{net::TcpStream, time::Duration};
use uuid::Uuid;
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let a: Vec<String> = std::env::args().collect();
    let root = std::path::Path::new("/home/build/apps/homesense/simulation");
    let mut identity = IdentityStore::open(root.join("vlan3/identity")).await?;
    let config = identity.load_config().await?;
    assert_eq!(config.name, "HomeSense Simulation");
    let credentials = root.join("network-probe-controller.json");
    if a[1] == "init" {
        assert!(!credentials.exists());
        let id = Uuid::new_v4();
        let key = ed25519_dalek::SigningKey::generate(&mut rand::rng());
        use std::os::unix::fs::OpenOptionsExt;
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new().create_new(true).write(true).mode(0o600).open(credentials)?;
        f.write_all(&hap::serde_json::to_vec(&(id, key.to_bytes()))?)?;
        identity.save_pairing(&Pairing::new(id, Permissions::Admin, key.verifying_key().to_bytes())).await?;
        println!("Temporary simulation controller registered");
        return Ok(());
    }
    let (id, bytes): (Uuid, [u8;32]) = hap::serde_json::from_slice(&std::fs::read(&credentials)?)?;
    if a[1] == "cleanup" {
        identity.delete_pairing(&id).await?;
        std::fs::remove_file(credentials)?;
        println!("Temporary controller removed"); return Ok(());
    }
    let key = ed25519_dalek::SigningKey::from_bytes(&bytes);
    let mut stream = TcpStream::connect_timeout(&a[1].parse()?, Duration::from_secs(5))?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    let mut session = support::verify(&mut stream, id, &key, &config.device_id.to_string(), &config.device_ed25519_keypair.verifying_key());
    let (headers, body) = session.request(&mut stream, b"GET /accessories HTTP/1.1\r\nHost: simulation\r\n\r\n");
    assert!(headers.starts_with("HTTP/1.1 200"));
    let accessories: hap::serde_json::Value = hap::serde_json::from_slice(&body)?;
    let mut target = None;
    for accessory in accessories["accessories"].as_array().unwrap() {
        for service in accessory["services"].as_array().unwrap() {
            for c in service["characteristics"].as_array().unwrap() {
                let t=c["type"].as_str().unwrap_or("");
                if t=="8" || t.starts_with("00000008-") { target=Some((accessory["aid"].as_u64().unwrap(),c["iid"].as_u64().unwrap())); }
            }
        }
    }
    let (aid,iid)=target.expect("simulated brightness");
    let expected: u8 = a[3].parse()?;
    if a[2]=="write" {
        let body=hap::serde_json::json!({"characteristics":[{"aid":aid,"iid":iid,"value":expected}]}).to_string();
        let req=format!("PUT /characteristics HTTP/1.1\r\nHost: simulation\r\nContent-Type: application/hap+json\r\nContent-Length: {}\r\n\r\n{}",body.len(),body);
        let (header,_) = session.request(&mut stream,req.as_bytes());
        assert!(header.starts_with("HTTP/1.1 204"),"{header}");
    }
    let req=format!("GET /characteristics?id={aid}.{iid} HTTP/1.1\r\nHost: simulation\r\n\r\n");
    let mut confirmed=false;
    for _ in 0..30 {
        std::thread::sleep(Duration::from_millis(100));
        let (_,body)=session.request(&mut stream,req.as_bytes());
        let v:hap::serde_json::Value=hap::serde_json::from_slice(&body)?;
        if v["characteristics"][0]["value"].as_u64()==Some(expected as u64) {confirmed=true;break;}
    }
    assert!(confirmed,"feedback did not confirm expected level");
    println!("{}",hap::serde_json::json!({"path":a[1],"operation":a[2],"confirmed_level":expected,"accessory_id":config.device_id.to_string(),"encrypted_hap":true}));
    Ok(())
}
