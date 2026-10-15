# Reviewed scene drafts

`reviewed_scenes::convert` takes an exact archived project, a parsed module-based
configuration, and a `Review`. It performs no I/O or state publication. The
`reviewed-scene-draft PROJECT.xml CONFIG.json REVIEW.json [EXACT_EVENT.xml]` binary
prints its result. Output contains **private raw source XML and identifiers**;
keep it local and outside a public Git tree.

The review binds the full project SHA-256 and configuration revision, supplies
explicit proxy-to-stable-logical-module-device binding reviews (including an explicit
standalone versus normalized combined-light behavior choice), and includes an
exact source-scene SHA-256 contract for each requested scene. Every indexed
source leaf needs its exact value, disposition, and nonempty review evidence.
The result preserves owner Agent, raw scene ID, qualified ID, display name,
source fields, full raw scene XML, member/element order and review dispositions.
It includes a complete strict runtime document with every generated scene
`enabled:false`, no rules, no trigger choice and no variable lifecycle guesses.

No contract is synthesized from source metadata. Missing contracts, bindings,
unknown fields/attributes, missing or stale field reviews, unsupported policies,
invalid strict-runtime definitions, or unresolved dependencies hold the whole
scene. There is no usable partial scene definition in a held result. Project or
configuration scope mismatch is a fatal error, as are malformed reviews or
invalid source inventory. A held dependency holds each dependent scene.

## Supported explicit contracts

* Delay unit is milliseconds, seconds or minutes. The anchor is absolute scene
  start or cumulative delay per source member. Overflow and excessive delays
  reject; original member/element order is retained, including repeated targets.
* Rate semantics are explicitly **duration** with a unit and a required zero
  policy (`zero_rate:immediate`). Nonzero rates require an explicit software fade
  backend with interval/lateness bounds and a supported dimmer profile. Hardware
  ramp duration is not approximated by an immediate target or a default Bx ramp.
  AD8 software fade and unsupported immediate intermediate AD8 levels hold.
* `nonoperative_tracking_tokens` is an explicit review that those exact source
  tokens have no runtime tracking in that reviewed scope. Alternatively, an
  explicit `TrackingContract` binds every source member and tracking field to a
  reviewed actual-feedback policy; see [SCENE-TRACKING.md](SCENE-TRACKING.md).
  Neither path assigns meaning to numeric tracking codes. A tracking contract
  emits disabled policies, preserves all control actions, and does not approve
  flags or other effects. Source colors, holds, flash, and full-on/full-off
  behavior still require separate support; only zero hold rates, false effect
  flags and neutral black color metadata are accepted in this conversion scope.
* An owned supply relay proxy remains forbidden as an independent scene member.
  Mixed source scenes containing both that relay and its logical combined dimmer
  hold in full with an `owned-supply/alias conflict` blocker. No relay node is
  removed, inverted, or replaced with restore-brightness behavior. Only a
  separately reviewed normalized combined-light level binding can use the
  existing logical planner, preserving one member source on both physical writes.
* Saved source `is_active` is retained as history, never used to initialize a
  runtime scene selection or a persistent variable.
* `deactivation:target` must match the exact raw `toggle_id` and resolve within
  the same source Agent. Its **complete target activation vector** becomes the
  source definition's deactivation, preserving target provenance. It never
  inverts source members or unions source and target memberships.
* `deactivation:unavailable` requires an explicit exact unresolved source value.
  It never substitutes all-off for a numeric sentinel. A resolved toggle target
  cannot be discarded as unavailable. Calls requiring unavailable deactivation
  are rejected. Deferred deactivation (source or target timeline/fade) requires
  a separately reviewed `deactivation_runtime` policy. Without it the entire
  source stays held. With `require_quiescent_runtime`, the converter emits a
  disabled qualified `scene_deactivations` link to the complete target activation,
  retaining its real timeline/fade instead of an immediate approximation.
  See [runtime contract](../../docs/reviewed-deactivation.md).

## Qualified call linkage and disabled gate

`link_event` verifies an exact unique event in the same project and revalidates
the scene document. Each ACTIVATE_SCENE/DEACTIVATE_SCENE is resolved using the
**code action target Agent plus SCENE_ID**; the calling event's device and a bare
scene ID are never namespaces. The existing strict event compiler verifies
owner type/ID versus SCENE_ID, retains If/Else and variable actions and the full
source-node chain, and suppresses disabled source code chains. The draft also
retains exact source event XML (including disabled chains), event fingerprint
and per-call qualified references. Any unresolved enabled call rejects the
whole event. Triggers and variable lifetimes still require separate review.

Strict immediate scenes now accept explicit `enabled:false`. The document keeps
these definitions for review but excludes them from enabled rule expansion and
scene-result trigger resolution, including caller-supplied scene maps. Merely
enabling a caller cannot enable a disabled scene. Older immediate definitions
that omitted `enabled` retain legacy behavior. The synthetic tests explicitly
enable definitions and callers only in Simulation; the converter never does.

## Validation boundaries

Tests exercise two Agents with the same bare scene ID and display name; source
4 members versus a disjoint target 36; repeated elements and explicit timing;
whole-scene failure propagation; false original conditions; strict disabled
gates; actual runtime routing and memory wire writes; and durable source chains
after database reopen. Separate mixed-owned-supply rejection and reviewed
combined-light positive cases verify the membership constraint; the independent
4-to-36 relay test alone does not cover mixed source scenes. These demonstrate generic mechanisms using synthetic
contracts, not approval or exact migration of any original household scene,
program, trigger, variable, physical model or control ownership. No original
source operation is enabled by these checks. Explicit actual-feedback tracking
has a separate policy and runtime contract; original tracking-code meanings,
source-specific colors/hold behavior, additional active-work deactivation policies,
and source-semantic confirmations remain pending. Reviewed Immediate definitions also have a direct
HAP/Web causal action entry; mixed Immediate/Timed/native-selection requests
retain the original provider, ordered commands, variable transactions and source
audit. See [the entry contract](../../docs/immediate-scene-entry.md).
