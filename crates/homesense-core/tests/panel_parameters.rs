use homesense_core::{
    Command, DeviceId, PanelParameter,
    queue::{CommandQueue, Epochs, Origin, QueuedCommand},
};
#[test]
fn panel_parameters_do_not_cancel_queued_led_rule_on_transport_anchor() {
    let epochs = Epochs {
        transport: 1,
        configuration: 1,
        control: 1,
    };
    let mut queue = CommandQueue::new(8, 2).unwrap();
    let id = DeviceId::new("legacy:1").unwrap();
    let led = QueuedCommand {
        id: 1,
        device_id: id.clone(),
        command: Command::PanelLed(true),
        origin: Origin::Rule,
        epochs,
        deadline_ms: 1000,
        run_id: Some(7),
    };
    assert!(queue.enqueue(led.clone(), 0, epochs).unwrap().is_empty());
    let setting = QueuedCommand {
        id: 2,
        device_id: id,
        command: Command::PanelParameter(PanelParameter::Sound(false)),
        origin: Origin::Hap,
        epochs,
        deadline_ms: 1000,
        run_id: None,
    };
    assert!(queue.enqueue(setting, 0, epochs).unwrap().is_empty());
    assert_eq!(queue.len(), 2);
    assert_eq!(queue.poll(1, epochs).next.unwrap(), led);
    assert!(matches!(
        queue.poll(2, epochs).next.unwrap().command,
        Command::PanelParameter(_)
    ));
}
