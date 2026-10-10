//! Offline tooling only; no sockets or physical device commands.
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
            "HomeSense offline tooling\nUsage: homesensed demo [0..100]\n       homesensed check-config FILE [--simulation]\n       homesensed decode 'AE 15 D1 1E EE'\nNo network service or hardware control is implemented yet."
        ),
        _ => return Err("unknown command; use --help".into()),
    }
    Ok(())
}
fn main() -> ExitCode {
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
