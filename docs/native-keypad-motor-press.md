# Native card motor presses

A reviewed six-key card may opt each Open/Close mapping into `press_behavior: "repeat_to_stop"`. Omission and `"absolute"` preserve existing behavior. Stop cannot opt in. This metadata affects only the native card; details, HAP, physical panel events and existing automatic rules keep their absolute commands.

```json
{"kind":"motor","device":"curtain","action":"open","press_behavior":"repeat_to_stop","review_evidence":"Reviewed native card mapping"}
```

The original `key.action` stays unchanged. An opted-in key additionally exposes `key.card_action`:

```json
{"kind":"motor_press","device":"curtain","behavior":"repeat_to_stop","command":{"action":"keypad_press","keypad":"panel","key":1},"path":"/api/v1/device-commands"}
```

Only the card posts that command, with the same target, current revision and a fresh `request_id` per click. Details continue to post the original Open/Close/Stop descriptor. A malformed non-null card descriptor must disable that card action; clients must not guess a direction or STOP from cached state, LEDs or a second request.

Admission resolves the exact configured enabled key and calibrated target. The opt-in target must have `curtain_state` to install the shared trajectory model; travel times may remain absent. Existing absolute mappings still allow targets without that model. Both the child endpoint and target must be authorized by the originating credential; the derived logical parent ID needs no additional grant. Both physical gateways must be current and ready. The decision and durable command acceptance share one owner turn: Open while the trusted Written estimate is Opening means Stop; Close while Closing means Stop; unknown, stopped or the opposite direction means the explicitly requested direction. Accepted/Writing work on that motor rejects another conditional press. Work on another motor does not block it. The independent absolute Stop remains available.

A same-key repeat is not proof of physical motion. Estimates are invalidated by restart, configuration/transport changes and loss of source authority. A STOP inferred from a previous credential's trajectory retains that exact source generation through acceptance. The normal request ledger records the result; the same `request_id` returns its recorded outcome without a second decision or write. Unknown outcomes are not automatically retried.

## Optional direction LED timeline

A separately reviewed `curtain_state.direction_led_policy` enables hints on configured LED bindings for every successful command source of that logical curtain:

```json
{"kind":"written_direction_delay","opposite_red_after_ms":2000}
```

Omission or `{"kind":"legacy"}` preserves the prior policy. The delay must be between 1 ms and 24 hours. In the new policy, the requested direction becomes red on actual motor Written; the opposite direction receives no output until the delay, then becomes red. Its prior red, green or unknown value is preserved during the delay. These are written hints, never physical LED observations.

Estimated full-open is Open green/Close red; full-closed is Close green/Open red; an intermediate Stop makes both red. These terminal states take precedence over a delayed hint. Stop LED remains red while moving and uses the existing nonzero `stop_led_idle_ms` after stopping; a reviewed 1 ms value is valid, zero remains invalid.

The owner's next deadline includes the delayed edge even without UI reads or bus feedback. A pending command freezes old hints until the next real Written. Reverse/repeat/Stop, invalidation and revocation cancel stale edges; queued hints also recheck the current cause and color at dispatch. No old timer is replayed after reconnect or allowed to overwrite a newer terminal state.

## Offline verification

`tools/verify_keypad_motor_press.py --binary /absolute/path/to/homesensed --sha256 SHA256 --output /absolute/new/output-directory` runs the ordinary production CLI with synthetic loopback TCP/TLS/WSS endpoints. It requires Python 3 standard libraries and OpenSSL, and creates private synthetic token/certificate/state files. No Node package, fixture download or real gateway is required. Do not point the harness at a deployed service. The tests check unchanged absolute descriptors, child permissions, request/revision validation, idempotent replay, repeated-direction STOP, reverse direction, estimated endpoint handling and the no-client-read delayed LED edge.
