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
        Some("publish-config") if args.len() == 4 => {
            let document = std::fs::read(&args[1])?;
            let revision = homesensed::configuration_publish::publish(&document, std::path::Path::new(&args[2]), args[3].parse()?)?;
            println!("Published configuration revision={revision}; no hardware connection; HAP identity unchanged");
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
            let action = match args[4].as_str() { "open" => homesense_core::MotorAction::Open, "close" => homesense_core::MotorAction::Close, _ => return Err("expected open or close".into()) };
            let device = DeviceId::new(args[3].clone())?;
            let mut bytes = Vec::new();
            std::fs::File::open(&args[1])?.take(4 * 1024 * 1024 + 1).read_to_end(&mut bytes)?;
            let report = tokio::runtime::Runtime::new()?.block_on(homesensed::motor_commission::run(&bytes, std::path::Path::new(&args[2]), &device, action, true))?;
            println!("{report}");
        }
        Some("panel-led") if args.len() == 6 && args[5] == "--coexistence-confirmed" => {
            let red = match args[4].as_str() { "red" => true, "green" => false, _ => return Err("LED color must be red or green".into()) };
            let device = DeviceId::new(args[3].clone())?;
            let mut bytes = Vec::new();
            std::fs::File::open(&args[1])?.take(4 * 1024 * 1024 + 1).read_to_end(&mut bytes)?;
            let report = tokio::runtime::Runtime::new()?.block_on(homesensed::panel_led::set(&bytes, std::path::Path::new(&args[2]), &device, red, true))?;
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
            std::fs::File::open(&args[1])?.take(4 * 1024 * 1024 + 1).read_to_end(&mut bytes)?;
            let address: std::net::SocketAddr = args[3].parse()?;
            let seconds: u64 = args[4].parse()?;
            tokio::runtime::Runtime::new()?.block_on(homesensed::live_service::serve_live(
                &bytes, std::path::Path::new(&args[2]), address,
                std::time::Duration::from_secs(seconds), true))?;
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
            "HomeSense offline tooling\nUsage: homesensed publish-config FILE STATE_DIR EXPECTED_REVISION\n       homesensed serve-live FILE STATE_DIR PRIVATE_IP:PORT SECONDS --coexistence-confirmed\n       homesensed demo [0..100]\n       homesensed check-config FILE [--simulation]\n       homesensed decode 'AE 15 D1 1E EE'\n       homesensed serve-simulation-lan FILE STATE_DIR PRIVATE_IP:PORT SECONDS\n       homesensed serve-simulation FILE STATE_DIR PORT SECONDS\n       homesensed motor-test FILE STATE_DIR DEVICE open|close --coexistence-confirmed (STOP after 3 seconds)\n       homesensed panel-led FILE STATE_DIR DEVICE red|green --coexistence-confirmed\n       homesensed query-parameter FILE GATEWAY HEX_ADDRESS backlight|led-self-reset|motor-interlock --model-confirmed --coexistence-confirmed\n       homesensed observe FILE GATEWAY SECONDS --coexistence-confirmed\n       homesensed query-status FILE GATEWAY HEX_ADDRESS CHANNEL --model-confirmed --coexistence-confirmed\nserve-simulation binds loopback; serve-simulation-lan advertises mDNS on the LAN. Both use only simulated hardware."
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
