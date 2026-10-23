//! Offline tools, simulation, and explicitly configured live gateway services.
use hb_protocol::{Decoded, Registry, StreamDecoder, encode_in_context};
use homesense_config::Configuration;
use homesense_core::admission::{Access, RuntimePolicy};
use homesense_core::arbiter::Arbiter;
use homesense_core::execution::Execution;
use homesense_core::queue::{Epochs, Origin, QueuedCommand};
use homesense_core::{
    BusAddress, Channel, Command, DeviceId, ExecutionContext, LevelTransition, ModuleSpec, Percent,
    ProtocolProfile, Verification,
};
use homesense_simulator::{Faults, Gateway};
use homesense_state::StateStore;
use std::{error::Error, io::Read, process::ExitCode};

fn hex_bytes(text: &str) -> Result<Vec<u8>, String> {
    let compact: String = text.chars().filter(|c| !c.is_ascii_whitespace()).collect();
    if compact.is_empty()
        || !compact.len().is_multiple_of(2)
        || !compact.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Err("expected an even number of hexadecimal digits, e.g. 'AE 15 D1 1E EE'".into());
    }
    compact
        .as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| {
            u8::from_str_radix(std::str::from_utf8(pair).map_err(|e| e.to_string())?, 16)
                .map_err(|e| e.to_string())
        })
        .collect()
}

fn simulate_light(level: Percent) -> Result<(), Box<dyn Error>> {
    let module = ModuleSpec::new(
        BusAddress::from_hex("15")?,
        ProtocolProfile::GtD8Rl,
        Verification::Simulated,
    )?;
    let id = DeviceId::new("simulation:light")?;
    let channel = Channel::new(1)?;
    let epochs = Epochs {
        transport: 1,
        configuration: 1,
        control: 1,
    };
    let mut states = StateStore::new(1, epochs.transport, 1, 16)?;
    states.register(id.clone(), module.kind())?;
    let mut gateway = Gateway::new(
        vec![module.clone()],
        Faults {
            reply_delay_ms: 20,
            output_chunk_size: 2,
            ..Faults::default()
        },
        16,
    )?;
    let mut registry = Registry::for_simulation();
    registry.register(module.clone())?;
    let command = Command::SetLevel {
        level,
        transition: LevelTransition::Immediate,
    };
    let frame = encode_in_context(&module, channel, command, ExecutionContext::Simulation)?;
    let request = QueuedCommand {
        id: 1,
        device_id: id.clone(),
        command,
        origin: Origin::Hap,
        epochs,
        deadline_ms: 1000,
        run_id: None,
    };
    // Offline fixture only: real adapters must resolve authentication and load
    // persisted policy. No session or storage service is claimed by this demo.
    let policy = RuntimePolicy {
        epochs,
        publishing: false,
        controller: true,
        gateway_ready: true,
        storage_healthy: true,
        manual_degraded_enabled: false,
        valid_configuration_loaded: true,
    };
    let access = Access {
        authorized: true,
        device_exists: true,
        device_verified: false,
        existing_direct_manual: false,
    };
    let mut arbiter = Arbiter::new(policy, 8, 2)?;
    arbiter.submit(request, access, 0)?;
    states.set_desired(&id, 1, command, 0)?;
    let queued = arbiter
        .dispatch(0, |_| access)?
        .command
        .ok_or("missing queued demo command")?;
    println!("HomeSense deterministic simulation; no hardware connection");
    println!("Queued {}: {:02X?}", queued.id, frame.bytes());
    let mut execution = Execution::new(queued.clone(), 0)?;
    execution.begin_write(epochs, 0)?;
    // Feed a split command into the simulated gateway, just like split TCP writes.
    for chunk in frame.bytes().chunks(2) {
        for result in gateway.receive(chunk, 0)? {
            result?;
        }
    }
    execution.written(0)?;
    arbiter.finish_write(queued.id, true)?;
    println!(
        "Written: observed={:?}, quality={:?}",
        states.get(&id).unwrap().observed,
        states.get(&id).unwrap().quality
    );
    let mut decoder = StreamDecoder::default();
    for chunk in gateway.drain_due(20)? {
        for frame in decoder.feed(&chunk) {
            if let Decoded::Feedback {
                address,
                channel: observed_channel,
                observation,
            } = registry.decode(frame)
            {
                if address != module.address() || observed_channel != channel {
                    return Err("unexpected simulated endpoint".into());
                }
                states.observe(&id, observation, epochs.transport, 20)?;
                if execution.observe(&id, observation, epochs.transport, 20)? {
                    states.finish_pending(&id, queued.id, 20)?;
                }
            }
        }
    }
    let state = states.get(&id).unwrap();
    if state.pending_command.is_some() {
        return Err("simulation did not observe the requested target".into());
    }
    println!(
        "Feedback: observed={:?}, quality={:?}, pending={:?}",
        state.observed, state.quality, state.pending_command
    );
    Ok(())
}

fn run(args: &[String]) -> Result<(), Box<dyn Error>> {
    match args.first().map(String::as_str) {
        Some("serve-automation-tcp") if args.len() >= 6 && args[5] == "--coexistence-confirmed" => {
            let read = |path: &str| -> Result<Vec<u8>, Box<dyn Error>> {
                let mut bytes = vec![];
                std::fs::File::open(path)?.take(4 * 1024 * 1024 + 1).read_to_end(&mut bytes)?;
                if bytes.len() > 4 * 1024 * 1024 { return Err("configuration exceeds 4 MiB".into()); }
                Ok(bytes)
            };
            let mut recoveries=None;
            let mut interval=None;
            let mut web=std::collections::BTreeMap::new();
            let mut hap_address=None;
            let mut hap_plan=None;
            let mut hap_motors=std::collections::BTreeSet::new();
            let mut hap_scenes=std::collections::BTreeSet::new();
            let (flags,remainder)=args[6..].as_chunks::<2>();
            for pair in flags {
                match pair[0].as_str() {
                    "--hap-plan" if hap_plan.is_none()=>hap_plan=Some(read(&pair[1])?),
                    "--hap-listen" if hap_address.is_none()=>hap_address=Some(pair[1].parse::<std::net::SocketAddr>()?),
                    "--hap-motor"=>{if !hap_motors.insert(homesense_core::DeviceId::new(pair[1].clone()).map_err(|e|format!("{e:?}"))?) || hap_motors.len()>64 {return Err("invalid or duplicate HAP motor".into());}},
                    "--hap-scene"=>{if pair[1].is_empty() || !hap_scenes.insert(pair[1].clone()) || hap_scenes.len()>homesensed::hap_endpoint::MAX_HAP_SCENES {return Err("invalid or duplicate HAP scene".into());}},
                    "--recoveries" if recoveries.is_none() => recoveries=Some(pair[1].parse::<u8>()?),
                    "--write-interval-ms" if interval.is_none() => interval=Some(pair[1].parse::<u64>()?),
                    "--web-listen"|"--web-origin"|"--web-cert"|"--web-key"|"--web-token"|"--web-role" => {
                        if web.insert(pair[0].clone(),pair[1].clone()).is_some(){return Err("duplicate management option".into());}
                    },
                    _ => return Err("unknown or duplicate automation option".into()),
                }
            }
            if !remainder.is_empty() {return Err("automation option requires a value".into());}
            if (!hap_scenes.is_empty() || !hap_motors.is_empty()) && hap_address.is_none(){return Err("HAP selections require --hap-listen".into());}
            if hap_plan.is_some() && (hap_address.is_some() || !hap_scenes.is_empty() || !hap_motors.is_empty()) {return Err("--hap-plan cannot be combined with single-bridge options".into());}
            if (hap_address.is_some() || hap_plan.is_some()) && web.is_empty(){return Err("combined HAP requires the Web TLS options".into());}
            let hap=hap_address.map(|address|homesensed::hap_endpoint::Settings{address,scenes:hap_scenes.into_iter().collect(),motors:hap_motors.into_iter().collect()});
            let interval=interval.unwrap_or(20);
            if !(20..=1000).contains(&interval) {return Err("write interval must be 20..1000 ms".into());}
            let management=if web.is_empty(){None}else{
                let mut required=|name:&str|web.remove(name).ok_or_else(||format!("missing management option {name}"));
                let address=required("--web-listen")?.parse()?;
                let origin=required("--web-origin")?;let certificate=required("--web-cert")?;let key=required("--web-key")?;let token=required("--web-token")?;
                let administrator=match web.remove("--web-role").as_deref().unwrap_or("member") {"member"=>false,"administrator"=>true,_=>return Err("Web role must be member or administrator".into())};
                let file=std::fs::File::open(token)?;let metadata=file.metadata()?;
                if !metadata.is_file() || metadata.len()>257{return Err("invalid credential file".into());}
                #[cfg(unix)]{use std::os::unix::fs::PermissionsExt;if metadata.permissions().mode()&0o077!=0{return Err("credential file must be private".into());}}
                let mut secret=String::new();file.take(257).read_to_string(&mut secret)?;
                Some(homesensed::service::WebSettings{address,secret:secret.trim_end().to_owned(),administrator,tls:Some(homesensed::web_tls::Settings::load(origin,std::path::Path::new(&certificate),std::path::Path::new(&key))?)})
            };
            let devices=read(&args[1])?;let rules=read(&args[2])?;let directory=std::path::Path::new(&args[3]);let mode=args[4].parse::<homesensed::lifecycle::RunMode>()?;let write_interval=std::time::Duration::from_millis(interval);
            tokio::runtime::Runtime::new()?.block_on(async {
                let mut signals=homesensed::process_signals::Signals::install().map_err(|e|e.to_string())?;
                let (stop,receive)=tokio::sync::watch::channel(false);
                let service=homesensed::automation_tcp::serve_running(&devices,&rules,directory,homesensed::lifecycle::RunControl::new(mode,receive),true,recoveries.unwrap_or(0),write_interval,homesensed::automation_tcp::Endpoints{management,hap,hap_plan});
                tokio::pin!(service);
                tokio::select! {
                    result=&mut service=>result,
                    signal=signals.wait()=>{signal?;stop.send_replace(true);service.await},
                }
            })?;
            println!("TCP automation stopped; state stored in automation-tcp.sqlite");
        }
        Some("serve-combined-simulation") if args.len()>=9 => {
            let read=|path:&str|->Result<Vec<u8>,Box<dyn Error>>{let mut bytes=vec![];std::fs::File::open(path)?.take(4*1024*1024+1).read_to_end(&mut bytes)?;Ok(bytes)};
            let file=std::fs::File::open(&args[7])?;
            let metadata=file.metadata()?;
            if !metadata.is_file() || metadata.len()>257 {return Err("invalid credential file".into());}
            #[cfg(unix)] {
                use std::os::unix::fs::PermissionsExt;
                if metadata.permissions().mode() & 0o077 != 0 {return Err("credential file must not be accessible by group or others".into());}
            }
            let mut secret=String::new();file.take(257).read_to_string(&mut secret)?;
            tokio::runtime::Runtime::new()?.block_on(homesensed::service::serve_combined_simulation(
                &read(&args[1])?,&read(&args[2])?,std::path::Path::new(&args[3]),std::time::Duration::from_secs(args[4].parse()?),
                homesensed::service::UnifiedSimulation{address:args[5].parse()?,scenes:args[8..].to_vec()},
                homesensed::service::WebSettings{address:args[6].parse()?,secret:secret.trim_end().to_owned(),administrator:false,tls:None}
            ))?;
            println!("Combined HAP/Web simulation stopped; no physical gateway connected");
        }
        Some("serve-unified-simulation") if args.len()>=7 => {
            let read=|path:&str|->Result<Vec<u8>,Box<dyn Error>>{let mut bytes=vec![];std::fs::File::open(path)?.take(4*1024*1024+1).read_to_end(&mut bytes)?;Ok(bytes)};
            tokio::runtime::Runtime::new()?.block_on(homesensed::service::serve_unified_simulation(&read(&args[1])?,&read(&args[2])?,std::path::Path::new(&args[3]),std::time::Duration::from_secs(args[4].parse()?),homesensed::service::UnifiedSimulation{address:args[5].parse()?,scenes:args[6..].to_vec()}))?;
            println!("Unified HAP simulation stopped; no physical gateway connected");
        }
        Some(command) if (command=="serve-web-simulation" && (args.len()==7 || (args.len()==8 && args[7]=="--administrator"))) || (command=="serve-web-tls-simulation" && (args.len()==10 || (args.len()==11 && args[10]=="--administrator"))) => {
            let read=|path:&str| -> Result<Vec<u8>,Box<dyn Error>> {let mut bytes=vec![];std::fs::File::open(path)?.take(4*1024*1024+1).read_to_end(&mut bytes)?;Ok(bytes)};
            let file=std::fs::File::open(&args[6])?;
            let metadata=file.metadata()?;
            if !metadata.is_file() || metadata.len()>257 {return Err("invalid credential file".into());}
            #[cfg(unix)] {
                use std::os::unix::fs::PermissionsExt;
                if metadata.permissions().mode() & 0o077 != 0 {return Err("credential file must not be accessible by group or others".into());}
            }
            let mut secret=String::new();file.take(257).read_to_string(&mut secret)?;
            let tls=if command=="serve-web-tls-simulation" {Some(homesensed::web_tls::Settings::load(args[7].clone(),std::path::Path::new(&args[8]),std::path::Path::new(&args[9]))?)} else {None};
            tokio::runtime::Runtime::new()?.block_on(homesensed::service::serve_web_simulation(
                &read(&args[1])?,&read(&args[2])?,std::path::Path::new(&args[3]),std::time::Duration::from_secs(args[4].parse()?),
                homesensed::service::WebSimulation{address:args[5].parse()?,secret:secret.trim_end().to_owned(),administrator:args.last().is_some_and(|v|v=="--administrator"),tls}
            ))?;
            println!("Web simulation stopped; no physical gateway connected");
        }
        Some("serve-automation-simulation") if args.len() == 5 || args.len() == 6 => {
            let read = |path: &str| -> Result<Vec<u8>, Box<dyn Error>> {
                let mut bytes = vec![];
                std::fs::File::open(path)?
                    .take(4 * 1024 * 1024 + 1)
                    .read_to_end(&mut bytes)?;
                Ok(bytes)
            };
            tokio::runtime::Runtime::new()?.block_on(
                homesensed::service::serve_automation_simulation(
                    &read(&args[1])?,
                    &read(&args[2])?,
                    std::path::Path::new(&args[3]),
                    std::time::Duration::from_secs(args[4].parse()?),
                    args.get(5).map(String::as_str),
                ),
            )?;
            println!("Automation simulation stopped; only automation-simulation.sqlite was used");
        }
        Some("publish-automation") if args.len() == 5 => {
            let read = |path: &str| -> Result<Vec<u8>, Box<dyn Error>> {
                let mut bytes = vec![];
                std::fs::File::open(path)?
                    .take(4 * 1024 * 1024 + 1)
                    .read_to_end(&mut bytes)?;
                Ok(bytes)
            };
            let revision = homesensed::configuration_publish::publish_automation(
                &read(&args[1])?,
                &read(&args[2])?,
                std::path::Path::new(&args[3]),
                args[4].parse()?,
            )?;
            println!(
                "Published device and automation configuration revision={revision}; runtime remains stopped; HAP identity unchanged"
            );
        }
        Some("publish-config") if args.len() == 4 => {
            let document = std::fs::read(&args[1])?;
            let revision = homesensed::configuration_publish::publish(
                &document,
                std::path::Path::new(&args[2]),
                args[3].parse()?,
            )?;
            println!(
                "Published configuration revision={revision}; no hardware connection; HAP identity unchanged"
            );
        }

        Some("query-status")
            if args.len() == 7
                && args[5] == "--model-confirmed"
                && args[6] == "--coexistence-confirmed" =>
        {
            let mut bytes = Vec::new();
            std::fs::File::open(&args[1])?
                .take(4 * 1024 * 1024 + 1)
                .read_to_end(&mut bytes)?;
            let config = Configuration::parse(&bytes, ExecutionContext::Live)?;
            let gateway = config.gateways().get(&args[2]).ok_or("unknown gateway")?;
            let address = BusAddress::from_hex(&args[3])?;
            let channel = Channel::new(args[4].parse()?)?;
            let module = gateway
                .modules()
                .get(&address)
                .ok_or("module not configured")?;
            let probe = homesensed::read_probe::ReadProbe::new(address, channel, module.profile())?;
            use std::io::Write;
            let mut output = std::io::stdout().lock();
            let mut emit = |value: hap::serde_json::Value| -> std::io::Result<()> {
                hap::serde_json::to_writer(&mut output, &value)?;
                writeln!(output)?;
                output.flush()
            };
            emit(
                hap::serde_json::json!({"event":"query_plan","endpoint":gateway.endpoint.to_string(),"profile":module.profile().id(),"raw":probe.bytes(),"configuration_unchanged":true}),
            )?;
            let mut stream = std::net::TcpStream::connect_timeout(
                &gateway.endpoint,
                std::time::Duration::from_secs(3),
            )?;
            stream.set_read_timeout(Some(std::time::Duration::from_millis(100)))?;
            stream.set_write_timeout(Some(std::time::Duration::from_millis(100)))?;
            let started = std::time::Instant::now();
            let outcome = probe.run(&mut stream, std::time::Duration::from_secs(2), |frame, elapsed| emit(hap::serde_json::json!({"event":"frame","raw":frame,"elapsed_ms":elapsed.as_millis() as u64})))?;
            emit(
                hap::serde_json::json!({"event":"query_finished","elapsed_ms":started.elapsed().as_millis() as u64,"outcome":format!("{outcome:?}"),"causal_acknowledgment":false}),
            )?;
            if !matches!(outcome, homesensed::read_probe::Outcome::Observation(_)) {
                return Err("query ended without matching observation".into());
            }
        }
        Some("motor-test") if args.len() == 6 && args[5] == "--coexistence-confirmed" => {
            let action = match args[4].as_str() {
                "open" => homesense_core::MotorAction::Open,
                "close" => homesense_core::MotorAction::Close,
                _ => return Err("expected open or close".into()),
            };
            let device = DeviceId::new(args[3].clone())?;
            let mut bytes = Vec::new();
            std::fs::File::open(&args[1])?
                .take(4 * 1024 * 1024 + 1)
                .read_to_end(&mut bytes)?;
            let report =
                tokio::runtime::Runtime::new()?.block_on(homesensed::motor_commission::run(
                    &bytes,
                    std::path::Path::new(&args[2]),
                    &device,
                    action,
                    true,
                ))?;
            println!("{report}");
        }
        Some("panel-led") if args.len() == 6 && args[5] == "--coexistence-confirmed" => {
            let red = match args[4].as_str() {
                "red" => true,
                "green" => false,
                _ => return Err("LED color must be red or green".into()),
            };
            let device = DeviceId::new(args[3].clone())?;
            let mut bytes = Vec::new();
            std::fs::File::open(&args[1])?
                .take(4 * 1024 * 1024 + 1)
                .read_to_end(&mut bytes)?;
            let report = tokio::runtime::Runtime::new()?.block_on(homesensed::panel_led::set(
                &bytes,
                std::path::Path::new(&args[2]),
                &device,
                red,
                true,
            ))?;
            println!("{report}");
        }
        Some("query-parameter")
            if args.len() == 7
                && args[5] == "--model-confirmed"
                && args[6] == "--coexistence-confirmed" =>
        {
            use homesensed::parameter_probe::{Parameter, Probe};
            let parameter = match args[4].as_str() {
                "backlight" => Parameter::Backlight,
                "led-self-reset" => Parameter::LedSelfReset,
                "motor-interlock" => Parameter::MotorInterlock,
                _ => return Err("unknown read-only parameter".into()),
            };
            let mut bytes = Vec::new();
            std::fs::File::open(&args[1])?
                .take(4 * 1024 * 1024 + 1)
                .read_to_end(&mut bytes)?;
            let config = Configuration::parse(&bytes, ExecutionContext::Live)?;
            let gateway = config.gateways().get(&args[2]).ok_or("unknown gateway")?;
            let address = BusAddress::from_hex(&args[3])?;
            let module = gateway
                .modules()
                .get(&address)
                .ok_or("module not configured")?;
            let probe = Probe::new(address, module.profile(), parameter)?;
            println!(
                "{}",
                hap::serde_json::json!({"event":"parameter_query","endpoint":gateway.endpoint.to_string(),"raw":probe.bytes(),"configuration_unchanged":true})
            );
            let mut stream = std::net::TcpStream::connect_timeout(
                &gateway.endpoint,
                std::time::Duration::from_secs(3),
            )?;
            stream.set_read_timeout(Some(std::time::Duration::from_millis(100)))?;
            stream.set_write_timeout(Some(std::time::Duration::from_millis(100)))?;
            let (raw, reply) = probe.run(&mut stream, std::time::Duration::from_secs(1))?;
            println!(
                "{}",
                hap::serde_json::json!({"event":"parameter_response","raw":raw,"interpretation":format!("{reply:?}"),"causal_acknowledgment":false})
            );
        }
        Some("observe") if args.len() == 5 && args[4] == "--coexistence-confirmed" => {
            let mut bytes = Vec::new();
            std::fs::File::open(&args[1])?
                .take(4 * 1024 * 1024 + 1)
                .read_to_end(&mut bytes)?;
            let config = Configuration::parse(&bytes, ExecutionContext::Live)?;
            let gateway = config.gateways().get(&args[2]).ok_or("unknown gateway")?;
            let duration = std::time::Duration::from_secs(args[3].parse()?);
            if duration.is_zero() || duration > std::time::Duration::from_secs(3600) {
                return Err("capture duration must be 1..3600 seconds".into());
            }
            let mut registry = Registry::default();
            for module in gateway.modules().values() {
                registry.register(module.clone())?;
            }
            let mut stream = std::net::TcpStream::connect_timeout(
                &gateway.endpoint,
                std::time::Duration::from_secs(3),
            )?;
            stream.set_read_timeout(Some(std::time::Duration::from_millis(100)))?;
            let mut output = std::io::stdout().lock();
            homesensed::diagnostics::capture(&mut stream, &registry, duration, 4096, |record| {
                use std::io::Write;
                hap::serde_json::to_writer(&mut output, &record)?;
                writeln!(output)?;
                output.flush()
            })?;
        }
        Some("serve-live") if args.len() == 6 && args[5] == "--coexistence-confirmed" => {
            let mut bytes = Vec::new();
            std::fs::File::open(&args[1])?
                .take(4 * 1024 * 1024 + 1)
                .read_to_end(&mut bytes)?;
            let address: std::net::SocketAddr = args[3].parse()?;
            let seconds: u64 = args[4].parse()?;
            tokio::runtime::Runtime::new()?.block_on(homesensed::live_service::serve_live(
                &bytes,
                std::path::Path::new(&args[2]),
                address,
                std::time::Duration::from_secs(seconds),
                true,
            ))?;
        }
        Some("serve-simulation-lan") if args.len() == 5 => {
            let mut bytes = Vec::new();
            std::fs::File::open(&args[1])?
                .take(4 * 1024 * 1024 + 1)
                .read_to_end(&mut bytes)?;
            let address: std::net::SocketAddr = args[3].parse()?;
            let seconds: u64 = args[4].parse()?;
            let runtime = tokio::runtime::Runtime::new()?;
            runtime.block_on(homesensed::service::serve_simulation_lan(
                &bytes,
                std::path::Path::new(&args[2]),
                address,
                std::time::Duration::from_secs(seconds),
            ))?;
        }
        Some("serve-simulation") if args.len() == 5 => {
            let mut bytes = Vec::new();
            std::fs::File::open(&args[1])?
                .take(4 * 1024 * 1024 + 1)
                .read_to_end(&mut bytes)?;
            let port: u16 = args[3].parse()?;
            let seconds: u64 = args[4].parse()?;
            let runtime = tokio::runtime::Runtime::new()?;
            runtime.block_on(homesensed::service::serve_simulation(
                &bytes,
                std::path::Path::new(&args[2]),
                std::net::SocketAddr::from(([127, 0, 0, 1], port)),
                std::time::Duration::from_secs(seconds),
            ))?;
        }
        Some("demo") if args.len() <= 2 => {
            let level = Percent::new(
                args.get(1)
                    .map(|s| s.parse::<u8>())
                    .transpose()?
                    .unwrap_or(30),
            )?;
            simulate_light(level)?;
        }
        Some("check-automation")
            if args.len() == 3 || (args.len() == 4 && args[3] == "--simulation") =>
        {
            let context = if args.len() == 4 {
                ExecutionContext::Simulation
            } else {
                ExecutionContext::Live
            };
            let read = |path: &str| -> Result<Vec<u8>, Box<dyn Error>> {
                let mut bytes = vec![];
                std::fs::File::open(path)?
                    .take(4 * 1024 * 1024 + 1)
                    .read_to_end(&mut bytes)?;
                Ok(bytes)
            };
            let config = Configuration::parse(&read(&args[1])?, context)?;
            let document = homesensed::configuration_publish::validate_automation(
                &config,
                &read(&args[2])?,
                context,
            )?;
            let linked = document.linked_rules_with_deferred()?;
            println!(
                "Automation structure and enabled command encoding valid: revision={}, rules={}, enabled={}, variables={}, immediate_scenes={}, schedules={}, gesture_keys={}, native_keypads={}, native_keys={}, context={:?}; no state created or connection opened",
                document.revision,
                document.rules.len(),
                linked.len(),
                document.variables.len(),
                document.immediate_scenes.len(),
                document.schedules.len(),
                document.gestures.len(),
                config.keypads().values().filter(|panel|panel.executes_events()).count(),
                config.keypads().values().filter(|panel|panel.executes_events()).map(|panel|panel.keys.len()).sum::<usize>(),
                context
            );
        }
        Some("list-backups") if args.len()==2 => {
            let entries=homesense_storage::backup::catalog(std::path::Path::new(&args[1]))?;
            println!("{}",serde_json::json!({"verified":false,"entries":entries.iter().map(|e|serde_json::json!({"name":e.name,"regular_file":e.regular_file,"bytes":e.bytes.to_string(),"modified_unix_ms":e.modified_unix_ms.map(|v|v.to_string())})).collect::<Vec<_>>()}));
        }
        Some("inspect-backup") if args.len()==2 || (args.len()==3 && args[2]=="--simulation") => {
            let context=if args.len()==3 {ExecutionContext::Simulation} else {ExecutionContext::Live};
            let report=homesense_storage::backup::inspect(std::path::Path::new(&args[1]),context)?;
            println!("{}",serde_json::json!({"integrity":"ok","schema":report.schema,"bytes":report.bytes,"active_revision":report.active_revision.map(|v|v.to_string()),"configuration_versions":report.configuration_versions,"automation_versions":report.automation_versions}));
        }
        Some("check-config")
            if args.len() == 2 || (args.len() == 3 && args[2] == "--simulation") =>
        {
            let context = if args.len() == 3 {
                ExecutionContext::Simulation
            } else {
                ExecutionContext::Live
            };
            let mut bytes = Vec::new();
            std::fs::File::open(&args[1])?
                .take(4 * 1024 * 1024 + 1)
                .read_to_end(&mut bytes)?;
            let config = Configuration::parse(&bytes, context)?;
            println!(
                "Configuration valid: revision={}, gateways={}, devices={}, enabled={}, context={:?}; no connection opened",
                config.revision(),
                config.gateways().len(),
                config.devices().len(),
                config.devices().values().filter(|d| d.enabled).count(),
                context
            );
        }
        Some("decode") if args.len() >= 2 => {
            let bytes = hex_bytes(&args[1..].join(" "))?;
            let mut decoder = StreamDecoder::default();
            for frame in decoder.feed(&bytes) {
                println!(
                    "Raw frame: {:02X?}; no registry classification",
                    frame.bytes()
                );
            }
            println!(
                "discarded={}, malformed={}, incomplete={}",
                decoder.discarded_bytes(),
                decoder.malformed_frames(),
                decoder.buffered_len()
            );
            if decoder.buffered_len() > 0
                || decoder.malformed_frames() > 0
                || decoder.discarded_bytes() > 0
            {
                return Err("input contains incomplete frames, noise or malformed frames".into());
            }
        }
        Some("--help" | "-h") | None => println!(
            "HomeSense offline tooling\nUsage: homesensed serve-combined-simulation DEVICE_FILE AUTOMATION_FILE STATE_DIR SECONDS HAP_LOOPBACK_IP:PORT WEB_LOOPBACK_IP:PORT TOKEN_FILE SCENE...\n       homesensed serve-web-tls-simulation DEVICE_FILE AUTOMATION_FILE STATE_DIR SECONDS LAN_IP:PORT TOKEN_FILE HTTPS_ORIGIN CERT_PEM KEY_PEM [--administrator]\n       homesensed serve-unified-simulation DEVICE_FILE AUTOMATION_FILE STATE_DIR SECONDS LOOPBACK_IP:PORT SCENE...\n       homesensed serve-web-simulation DEVICE_FILE AUTOMATION_FILE STATE_DIR SECONDS LOOPBACK_IP:PORT TOKEN_FILE [--administrator]\n       homesensed serve-automation-tcp DEVICE_FILE AUTOMATION_FILE STATE_DIR SECONDS|forever --coexistence-confirmed [--recoveries 0..16] [--write-interval-ms 20..1000] [--hap-plan PLAN_FILE] [--hap-listen PRIVATE_IP:PORT [--hap-scene SCENE]... [--hap-motor DEVICE]...] [--web-listen LAN_IP:PORT --web-origin HTTPS_ORIGIN --web-cert CERT_PEM --web-key KEY_PEM --web-token TOKEN_FILE --web-role member|administrator]\n       homesensed serve-automation-simulation DEVICE_FILE AUTOMATION_FILE STATE_DIR SECONDS [SCENE]\n       homesensed publish-config FILE STATE_DIR EXPECTED_REVISION\n       homesensed publish-automation DEVICE_FILE AUTOMATION_FILE STATE_DIR EXPECTED_REVISION\n       homesensed serve-live FILE STATE_DIR PRIVATE_IP:PORT SECONDS --coexistence-confirmed\n       homesensed demo [0..100]\n       homesensed list-backups DIRECTORY\n       homesensed inspect-backup FILE [--simulation]\n       homesensed check-config FILE [--simulation]\n       homesensed check-automation DEVICE_FILE AUTOMATION_FILE [--simulation]\n       homesensed decode 'AE 15 D1 1E EE'\n       homesensed serve-simulation-lan FILE STATE_DIR PRIVATE_IP:PORT SECONDS\n       homesensed serve-simulation FILE STATE_DIR PORT SECONDS\n       homesensed motor-test FILE STATE_DIR DEVICE open|close --coexistence-confirmed (STOP after 3 seconds)\n       homesensed panel-led FILE STATE_DIR DEVICE red|green --coexistence-confirmed\n       homesensed query-parameter FILE GATEWAY HEX_ADDRESS backlight|led-self-reset|motor-interlock --model-confirmed --coexistence-confirmed\n       homesensed observe FILE GATEWAY SECONDS --coexistence-confirmed\n       homesensed query-status FILE GATEWAY HEX_ADDRESS CHANNEL --model-confirmed --coexistence-confirmed\nserve-simulation binds loopback; serve-simulation-lan advertises mDNS on the LAN. Both use only simulated hardware."
        ),
        _ => return Err("unknown command; use --help".into()),
    }
    Ok(())
}
fn main() -> ExitCode {
    // Keep diagnostics to protocol stages/statuses, never request bodies or keys.
    let mut logger = env_logger::Builder::new();
    logger.parse_filters("warn,hap::transport::http=info,homesensed::reconnect=info,homesensed::runtime_hap=info,homesensed::live_service=info");
    if std::env::var("HOMESENSE_LIGHT_DIAGNOSTICS").as_deref() == Ok("1") {
        logger.filter_module("homesense_hap", log::LevelFilter::Debug);
        logger.filter_module("homesensed::control_batch", log::LevelFilter::Debug);
        logger.filter_module("homesensed::admission", log::LevelFilter::Debug);
        logger.filter_module("homesensed::automation_drive::indicator_waves", log::LevelFilter::Debug);
        logger.filter_module("homesensed::automation_drive", log::LevelFilter::Debug);
        logger.filter_module("homesensed::away_execution", log::LevelFilter::Debug);
    }
    logger.init();
    match run(&std::env::args().skip(1).collect::<Vec<_>>()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("homesensed: {error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hex_input_is_strict_and_accepts_whitespace() {
        assert_eq!(
            hex_bytes("AE 15\nD1 1e EE").unwrap(),
            [0xAE, 0x15, 0xD1, 30, 0xEE]
        );
        for input in ["", "AE1", "0xAE", "AE💡", "AE GG"] {
            assert!(hex_bytes(input).is_err());
        }
    }
}
