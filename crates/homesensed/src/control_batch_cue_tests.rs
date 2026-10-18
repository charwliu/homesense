use super::*;
use homesense_core::ExecutionContext;
use homesense_hap::requests::Intent;

fn config() -> Configuration {
    Configuration::parse(include_bytes!("../../../config/test/native-app-keypad/devices.json"), ExecutionContext::Simulation).unwrap()
}
fn intent(id: &str, command: Command) -> Intent {
    Intent { device: DeviceId::new(id).unwrap(), command }
}
fn light(level: u8) -> Command {
    Command::SetLevel { level: homesense_core::Percent::new(level).unwrap(), transition: homesense_core::LevelTransition::Immediate }
}

#[test]
fn final_origin_color_keeps_its_provenance_without_coalescing_other_keys() {
    let config = config();
    for final_color in [Command::PanelLed(true), Command::PanelLed(false), Command::PanelLedOff] {
        let mut intents = vec![
            intent("legacy-key-1", Command::PanelLed(false)),
            intent("legacy-key-2", Command::PanelLed(true)),
            intent("fixture-light", light(100)),
            intent("legacy-key-2", Command::PanelLed(false)),
            intent("legacy-key-1", final_color),
        ];
        let mut sources = Some((0..5).map(|i| i.to_string()).collect());
        let local = coalesce_local_cue(&config, &mut intents, &mut sources, Some(4));
        assert_eq!(local, Some(3));
        assert_eq!(sources.as_ref().unwrap(), &["1", "2", "3", "4"]);
        assert_eq!(intents[0].command, Command::PanelLed(true));
        assert_eq!(intents[2].command, Command::PanelLed(false));
        let permutation = partition_local(&config, &mut intents, &mut sources, local);
        assert_eq!(permutation, vec![3, 1, 0, 2]);
        assert_eq!(intents[0].command, final_color);
        assert_eq!(sources.as_ref().unwrap(), &["4", "2", "1", "3"]);
    }
}

#[test]
fn query_toggle_pause_and_panel_parameters_keep_prior_color_on_their_side() {
    let config = config();
    for barrier in [
        intent("fixture-light", Command::Query),
        intent("fixture-light", Command::Toggle),
        intent("fixture-light", Command::PauseRamp),
        intent("legacy-key-1", Command::PanelParameter(homesense_core::PanelParameter::ReadBacklight)),
    ] {
        let mut intents = vec![
            intent("legacy-key-1", Command::PanelLed(false)), barrier,
            intent("legacy-key-1", Command::PanelLedOff),
            intent("fixture-light", light(0)),
            intent("legacy-key-1", Command::PanelLed(true)),
        ];
        let mut sources = Some((0..5).map(|i| i.to_string()).collect());
        assert_eq!(coalesce_local_cue(&config, &mut intents, &mut sources, Some(4)), Some(3));
        assert_eq!(intents.len(), 4);
        assert_eq!(intents[0].command, Command::PanelLed(false));
        assert_eq!(sources.unwrap(), ["0", "1", "3", "4"]);
    }
}

#[test]
fn no_authenticated_origin_does_not_coalesce_any_color() {
    let mut intents = vec![intent("legacy-key-1", Command::PanelLed(false)), intent("legacy-key-1", Command::PanelLed(true))];
    let mut sources = Some(vec!["first".into(), "last".into()]);
    assert_eq!(coalesce_local_cue(&config(), &mut intents, &mut sources, None), None);
    assert_eq!(intents.len(), 2);
    assert_eq!(sources.unwrap(), ["first", "last"]);
}
