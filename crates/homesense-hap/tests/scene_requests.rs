use homesense_hap::{
    requests::RequestError, scene_graph::SceneBinding, scene_requests::SceneRequests,
};
#[test]
fn actions_are_explicit_and_reset_and_subscriptions_still_require_authorization() {
    let requests = SceneRequests::new(vec![SceneBinding {
        scene: "reading".into(),
        aid: 2,
        on_iid: 9,
    }])
    .unwrap();
    let controller = uuid::Uuid::new_v4();
    for value in ["true", "1", "false", "0"] {
        let body = format!(r#"{{"characteristics":[{{"aid":2,"iid":9,"value":{value}}}]}}"#);
        let batch = requests.normalize(controller, body.as_bytes()).unwrap();
        assert_eq!(batch.controller, controller);
        assert_eq!(batch.addressed, vec!["reading"]);
        assert_eq!(batch.values["reading"], value == "true" || value == "1");
        assert_eq!(
            batch.starts.len(),
            usize::from(value == "true" || value == "1")
        );
    }
    let batch = requests
        .normalize(
            controller,
            br#"{"characteristics":[{"aid":2,"iid":9,"ev":true}]}"#,
        )
        .unwrap();
    assert!(batch.starts.is_empty());
    assert!(batch.values.is_empty());
    assert_eq!(batch.addressed, vec!["reading"]);
    assert_eq!(batch.subscriptions.len(), 1);
    for (body, expected) in [
        (
            r#"{"characteristics":[{"aid":2,"iid":9,"value":2}]}"#,
            RequestError::InvalidValue,
        ),
        (
            r#"{"characteristics":[{"aid":2,"iid":9,"value":true},{"aid":2,"iid":9,"value":false}]}"#,
            RequestError::Duplicate,
        ),
        (
            r#"{"characteristics":[{"aid":2,"iid":9,"value":true},{"aid":3,"iid":9,"value":true}]}"#,
            RequestError::UnknownCharacteristic,
        ),
        (
            r#"{"characteristics":[{"aid":2,"iid":9,"value":true,"authData":"x"}]}"#,
            RequestError::UnsupportedAuthorization,
        ),
    ] {
        assert_eq!(
            requests.normalize(controller, body.as_bytes()).unwrap_err(),
            expected
        );
    }
}
