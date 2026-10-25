//! Synthetic protocol vectors without a physical connection or control grant.
use hb_protocol::{Decoded, Registry, StreamDecoder};
use homesense_core::{BusAddress, ModuleSpec, Observation, ProtocolProfile, Verification};
#[test]
fn synthetic_ts6_events_survive_every_fragment_size_without_losing_releases() {
    let expected_keys = [3, 2, 6, 4, 6, 4, 3, 3, 4, 2, 6, 4];
    let raw: Vec<u8> = expected_keys
        .iter()
        .flat_map(|key| {
            [0xf1, 0xf2]
                .into_iter()
                .flat_map(move |value| [0xae, 0x41, 0xd0 + key, value, 0xee])
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
                other => panic!("unexpected synthetic event: {other:?}"),
            }
        }
    }
}
