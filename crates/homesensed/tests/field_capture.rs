//! Replay immutable site evidence without a physical connection or control grant.
use hb_protocol::{Decoded, Registry, StreamDecoder};
use homesense_core::{BusAddress, ModuleSpec, Observation, ProtocolProfile, Verification};
#[test]
fn recorded_ts6_events_survive_every_fragment_size_without_losing_releases() {
    let rows: Vec<hap::serde_json::Value> =
        include_str!("../../../deployment/nanopc/evidence/20261010-a3/panel-a3-listen.jsonl")
            .lines()
            .map(|s| hap::serde_json::from_str(s).unwrap())
            .collect();
    assert_eq!(rows.last().unwrap()["frames"], 24);
    let raw: Vec<u8> = rows
        .iter()
        .filter(|r| r["event"] == "frame")
        .flat_map(|r| {
            r["raw"]
                .as_array()
                .unwrap()
                .iter()
                .map(|b| u8::try_from(b.as_u64().unwrap()).unwrap())
        })
        .collect();
    let mut registry = Registry::for_simulation();
    registry
        .register(
            ModuleSpec::new(
                BusAddress::new(0x41).unwrap(),
                ProtocolProfile::Ts686Rs485SixKey,
                Verification::Simulated,
            )
            .unwrap(),
        )
        .unwrap();
    let expected_keys = [3, 2, 6, 4, 6, 4, 3, 3, 4, 2, 6, 4];
    for size in 1..=raw.len() {
        let mut decoder = StreamDecoder::default();
        let events: Vec<_> = raw
            .chunks(size)
            .flat_map(|chunk| decoder.feed(chunk))
            .map(|frame| registry.decode(frame))
            .collect();
        assert_eq!(events.len(), 24);
        for (index, event) in events.iter().enumerate() {
            match event {
                Decoded::Feedback {
                    address,
                    channel,
                    observation,
                } => {
                    assert_eq!(address.get(), 0x41);
                    assert_eq!(channel.get(), expected_keys[index / 2]);
                    assert_eq!(
                        *observation,
                        Observation::Button {
                            pressed: index % 2 == 0
                        }
                    );
                }
                other => panic!("unexpected recorded event: {other:?}"),
            }
        }
    }
}
