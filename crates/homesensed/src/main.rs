//! Offline tooling only; no sockets or physical device commands.
use hb_protocol::{Registry, StreamDecoder, encode_in_context};
use homesense_core::{
    BusAddress, Channel, Command, ExecutionContext, LevelTransition, ModuleSpec, Percent,
    ProtocolProfile, Verification,
};
use std::{error::Error, process::ExitCode};

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

fn run(args: &[String]) -> Result<(), Box<dyn Error>> {
    match args.first().map(String::as_str) {
        Some("demo") if args.len() == 1 => {
            let module = ModuleSpec::new(
                BusAddress::from_hex("15")?,
                ProtocolProfile::GtD8Rl,
                Verification::Simulated,
            )?;
            let frame = encode_in_context(
                &module,
                Channel::new(1)?,
                Command::SetLevel {
                    level: Percent::new(30)?,
                    transition: LevelTransition::Immediate,
                },
                ExecutionContext::Simulation,
            )?;
            println!("HomeSense offline demo; no hardware connection");
            println!("Encoded command: {:02X?}", frame.bytes());
            let mut registry = Registry::for_simulation();
            registry.register(module)?;
            let mut decoder = StreamDecoder::default();
            // Synthetic feedback is explicitly independent of the encoded command.
            // Encoding/writing alone must never confirm the observed state.
            let simulated_reply = [0xAE, 0x15, 0xD1, 0x1E, 0xEE];
            assert!(decoder.feed(&simulated_reply[..2]).is_empty());
            for reply in decoder.feed(&simulated_reply[2..]) {
                println!("Synthetic feedback: {:?}", registry.decode(reply));
            }
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
            "HomeSense offline tooling\nUsage: homesensed demo\n       homesensed decode 'AE 15 D1 1E EE'\nNo network service or hardware control is implemented yet."
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
