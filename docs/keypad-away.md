# Native keypad Away binding

An enabled, reviewed TS6 key can map to the existing module-level Away operation:

```json
{"kind":"away_off","review_evidence":"Reviewed native keypad operation","indicator_reset":"green"}
```

`indicator_reset` is optional; only `green` is supported and its target is the same configured key endpoint. It requires the reviewed manual LED mode. Omission schedules no LED write. This mapping does not install physical panel rules or emit a fabricated button event.

The runtime publishes `key.action = {"kind":"away_off","path":"/api/v1/away-off"}` and a card-only `key.card_action` with the same kind/path plus `command = {"action":"keypad_press","keypad":"panel","key":6}`. The native card posts that command with the current revision and a fresh request ID. Ordinary direct Away requests omit the command and retain their existing behavior. Clients cannot supply module lists, output scopes, LED targets or credentials in the body.

Activation requires the exact child endpoint and the entire reviewed Away scope under the original credential. Hidden supplies still require their enabled logical owners. The logical parent needs no extra grant. `key.can_activate` and `key.action_gate.available` reflect the key, full target permission, enabled policy and parent/target gateway readiness; otherwise action descriptors are hidden. Scope and credential generation are checked again for each actual write.

One admission commits the module command receipts, all per-output effects and any affected lighting cancellation atomically. The optional green reset is a normal audited tail command; it cannot dispatch until every module receipt is Written or Observed. A partial failure, revocation, changed generation or restart cannot replay it. The HTTP receipt remains `accepted`, with `physical_result_confirmed:false`; green means the key reset was written, not that every load has confirmed OFF. Actual output feedback and LED observations remain separate. Duplicate request IDs return the original outcome, and key context participates in the fingerprint.

## Daily catalog visibility

`devices[].catalog_visible` defaults to `true`. Setting it to `false` leaves the output enabled, polled, authorized and available for Away, scenes, module management and direct authorized API control. GET `/api/v1/devices` and WSS retain all authorized rows with the flag; the native daily directory filters false rows while its state store keeps scene references and control facts. `/api/v1/rooms` uses visible devices and accessible logical parents. Configuration exports and drafts preserve the input flag.

This is a native display hint, not an ACL or an implicit HAP exposure policy. Existing explicit HAP selections remain unchanged. Full Away coverage still needs every physical output enabled and reviewed, and complete originating authority; display filtering cannot substitute for that scope.
