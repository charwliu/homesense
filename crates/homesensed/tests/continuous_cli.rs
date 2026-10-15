//! Production CLI signal handling against generated, query-capable loopback hardware.
#![cfg(unix)]
#[path="../../test-support/cli.rs"]mod test_cli;
#[path="../../test-support/fixtures.rs"]mod fixtures;
use std::{io::{Read,Write},net::TcpListener,process::{Command,Stdio},sync::{Arc,Mutex},time::{Duration,Instant}};
struct Child(Option<std::process::Child>);
impl Drop for Child {fn drop(&mut self){if let Some(child)=self.0.as_mut(){let _=child.kill();let _=child.wait();}}}
#[test]
fn explicit_forever_runs_until_sigterm_or_sigint_and_finite_mode_still_expires(){
    for signal in [Some("-TERM"),Some("-INT"),None]{
        let directory=fixtures::Directory::new("continuous-cli");let listener=TcpListener::bind("127.0.0.1:0").unwrap();let address=listener.local_addr().unwrap();
        let mut devices:serde_json::Value=serde_json::from_slice(include_bytes!("../../../config/simulation.json")).unwrap();devices["gateways"][0]["endpoint"]=address.to_string().into();devices["gateways"][0]["modules"][0]["verification"]=serde_json::json!({"status":"verified","evidence":"synthetic production CLI","verified_at_unix_ms":1});
        let input=directory.write("devices.json",&serde_json::to_vec(&devices).unwrap());
        let rules=directory.write("rules.json",br#"{"schema_version":1,"configuration_revision":1,"variables":[],"rules":[{"id":"boot","enabled":true,"review_evidence":"synthetic","source":"synthetic","trigger":{"kind":"startup_synchronized"},"actions":[{"source":"synthetic","action":{"kind":"command","device":"simulation:light","command":{"kind":"level","value":25,"ramp":false}}}]}]}"#);
        let levels=Arc::new(Mutex::new(vec![]));let observed=levels.clone();let (ready,wait)=std::sync::mpsc::channel();
        let gateway=std::thread::spawn(move || {let (mut socket,_)=listener.accept().unwrap();socket.set_read_timeout(Some(Duration::from_secs(7))).unwrap();let mut level=0;loop{
            let mut frame=[0;5];match socket.read_exact(&mut frame){Ok(())=>{},Err(error) if matches!(error.kind(),std::io::ErrorKind::UnexpectedEof|std::io::ErrorKind::ConnectionReset)=>break,Err(error)=>panic!("synthetic CLI bus: {error}")}
            assert_eq!(frame[0],0xae);assert_eq!(frame[1],0x15);assert_eq!(frame[4],0xee);
            let reply=if frame[2]==0xa0{assert_eq!(frame[3],0xf2);(1..=8).flat_map(|channel|[0xae,0x15,0xd0+channel,level,0xee]).collect::<Vec<_>>()}else{assert_eq!(frame[2],1);level=frame[3];observed.lock().unwrap().push(level);let _=ready.send(());vec![0xae,0x15,0xd1,level,0xee]};
            if socket.write_all(&reply).is_err(){break;}
        }});
        let mode=if signal.is_some(){"forever"}else{"1"};let started=Instant::now();
        let mut child=Child(Some(Command::new(test_cli::binary(env!("CARGO_BIN_EXE_homesensed"))).arg("serve-automation-tcp").arg(&input).arg(&rules).arg(directory.path().join("state")).arg(mode).arg("--coexistence-confirmed").stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap()));
        wait.recv_timeout(Duration::from_secs(4)).expect("production CLI must synchronize and execute exactly one startup");
        if let Some(signal)=signal{
            std::thread::sleep(Duration::from_millis(1200));assert!(child.0.as_mut().unwrap().try_wait().unwrap().is_none(),"forever must not be a short finite mode");
            assert!(Command::new("kill").args([signal,&child.0.as_ref().unwrap().id().to_string()]).status().unwrap().success());
        }
        let deadline=Instant::now()+Duration::from_secs(4);while child.0.as_mut().unwrap().try_wait().unwrap().is_none(){assert!(Instant::now()<deadline,"CLI did not cleanly stop");std::thread::sleep(Duration::from_millis(10));}
        let output=child.0.take().unwrap().wait_with_output().unwrap();assert!(output.status.success(),"{}",String::from_utf8_lossy(&output.stderr));assert!(String::from_utf8_lossy(&output.stdout).contains("TCP automation stopped"));assert!(started.elapsed()>=Duration::from_secs(1));
        gateway.join().unwrap();assert_eq!(*levels.lock().unwrap(),vec![25],"shutdown must not replay startup or controls");
        let db=rusqlite::Connection::open(directory.path().join("state/automation-tcp.sqlite")).unwrap();let count:i64=db.query_row("SELECT count(*) FROM command_journal",[],|row|row.get(0)).unwrap();assert_eq!(count,1);let pending:i64=db.query_row("SELECT count(*) FROM command_journal WHERE status IN ('accepted','writing')",[],|row|row.get(0)).unwrap();assert_eq!(pending,0);
    }
}
#[test]
fn invalid_lifetimes_are_rejected_before_connecting_or_creating_state(){
    let directory=fixtures::Directory::new("continuous-cli-invalid");let listener=TcpListener::bind("127.0.0.1:0").unwrap();listener.set_nonblocking(true).unwrap();let mut devices:serde_json::Value=serde_json::from_slice(include_bytes!("../../../config/simulation.json")).unwrap();devices["gateways"][0]["endpoint"]=listener.local_addr().unwrap().to_string().into();
    let input=directory.write("devices.json",&serde_json::to_vec(&devices).unwrap());let rules=directory.write("rules.json",br#"{"schema_version":1,"configuration_revision":1,"variables":[],"rules":[]}"#);
    for mode in ["0","86401","18446744073709551615","-1","Forever"]{
        let state=directory.path().join("state");let output=Command::new(test_cli::binary(env!("CARGO_BIN_EXE_homesensed"))).arg("serve-automation-tcp").arg(&input).arg(&rules).arg(&state).arg(mode).arg("--coexistence-confirmed").output().unwrap();assert!(!output.status.success());assert!(!state.exists());assert!(listener.accept().is_err_and(|error|error.kind()==std::io::ErrorKind::WouldBlock));
    }
}
