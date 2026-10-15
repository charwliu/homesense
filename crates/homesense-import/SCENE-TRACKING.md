# Explicit actual scene tracking

The strict automation document accepts `scene_tracking` policies and separate
`scene_metadata` display labels. Tracking activity is independent of selected
scene intent, archived `is_active`, completed writes and final scene targets.
Neither tracking reads nor notifications send load or motor commands, set scene
variables, reconcile panel LEDs, or claim control of source programs.

A policy requires every field below; none of these behavior choices has a
default inferred from a source tracking number:

```json
{
  "scene": "stable-scene-id",
  "enabled": false,
  "aggregation": "all",
  "unknown_policy": "require_all_known",
  "pending_policy": "block_scene",
  "control_policy": "separate_action_and_read_only_activity",
  "activity_name": "Explicit activity label",
  "max_age_ms": 5000,
  "review_evidence": "explicit review reference",
  "members": [
    {
      "id": "stable-member-id",
      "device": "stable-logical-light-id",
      "predicate": {"kind": "level_range", "minimum": 20, "maximum": 60},
      "review_evidence": "explicit powered actual brightness predicate"
    }
  ]
}
```

`aggregation` is `all` or `any`. `unknown_policy:require_all_known` yields
Unknown if any relevant member lacks reliable feedback. `decisive_known` uses
three-valued logic: a known false decides All, a known true decides Any;
otherwise missing feedback remains Unknown. Explicitly ignored members do not
participate, and ignoring every member is invalid. `pending_policy:block_scene`
blocks the entire activity projection when a relevant member or a combined
light component has unresolved/ambiguous output. Ignored unrelated pending
outputs do not participate. Pending and Unknown are never emitted as false.

Member predicates are `power` (explicit `on` boolean), `level_range` (inclusive
1..100, dimmers only), and `ignore` with a review reason. Actual brightness is
exposed and used only while reliable logical power is On. A dark combined light
never supplies its remembered or physical dimmer setting as active brightness.

## Source review and boundaries

The converter's optional `TrackingContract` binds each exact source member index
in original order to one explicit predicate or ignore disposition. Every raw
tracking field still needs its exact expected value, `tracking` disposition and
review evidence. Complete scene/project fingerprints and reviewed logical
bindings remain mandatory. There is no implementation-defined mapping for
source tracking modes, tracking levels or unknown strategies. Incomplete,
reordered, stale or unsupported contracts hold the whole scene.

The converter emits complete but **disabled** policies and definitions, no
rules, and separate stable-ID/display-name metadata. Ignoring a member for
tracking never removes its source control action. Tracking review does not
approve source hold rates, colors, flags, timing, hardware ramp duration or
deferred deactivation. Unsupported behavior still holds the scene. A policy
cannot be enabled over a disabled definition/output or an existing selection,
reset or shutdown provider. Existing reviewed latching/native scene behavior
retains its provider.

## Runtime, reads and notifications

`Feedback.plan_rule` obtains canonical module/channel truth through reviewed
logical aliases and combined-light projection, with no recorded-brightness
input. Admission revalidates boot, revision, transport/control epochs, feedback
lease freshness and durable pending writes. Runtime readiness and authority are
checked before and after the proof. Missing, stale, future, foreign-boot,
foreign-generation, disconnected, revoked or publication-isolated input never
proves Active. A read does not initialize a selection variable.

The existing Control `SceneObservation` stream carries tracking activity plus
its feedback validity deadline. Web reads expose `activity_source`, member
observations and the separate `selected` field; tracking cards expose read-only activity alongside the independent execute
action, and do not offer a stateful activity On/Off control. Display labels remain separate from stable
control IDs.

HAP publication has two explicitly distinct roles. The existing instant action
keeps its Scene identity, AID/IID and momentary execution semantics. Its On
value is a trigger acknowledgement, never actual activity. A new typed
SceneActivity identity publishes read-only actual activity, with an explicit
reviewed activity label. It uses a separate storage namespace and separate
binding/handler type, never a scene-ID suffix or a name-based authorization key.
Both roles authorize by the canonical logical scene ID. The public activity
graph is read-only from construction; its permissions omit PairedWrite. The
activity batch handler and SceneActivityWrites control path reject true/false,
including false after a true read. The action handler cannot normalize activity
addresses and vice versa. Web keeps a working independent execute button while
activity stays separate from selected intent.

A writable stateful activity switch requires a future complete reviewed
deactivation/off policy. No off vector is inferred. Current independent actions
execute the already-reviewed scene definition; they do not promise that false
on a read-only status will turn loads off. Action receipts retain the canonical
scene identity; reading or writing the status role cannot create an action run.

HAP GETs use current runtime reads. HAP event notifications consume the live
SceneObservation subscription, not a GET refresh or selection cache. A bounded
projection timer processes updates even without new physical snapshot messages.
Activity changes emit boolean events; unknown, pending, expired, closed,
revoked or scope-invalid observations emit unavailable/null events. Recovery
re-emits the actual boolean even when it equals the last pre-failure value.
Repeated identical observations are deduplicated. Notifications are coalesced,
not a lossless event history. Bootstrap retries are rate-limited and cancelled by bridge teardown; HAP tracking publication is limited to 16 subscriptions.

## Immediate action adapter

The second independent action layer publishes reviewed, explicitly enabled
Immediate definitions through HAP and the Web scene catalog. It uses the
ordinary ordered rule/variable/causal admission path without converting a
scene into TimedScene. Read-only activity remains a separate typed identity.
Immediate acceptance reports causal/command receipts rather than inventing a
timeline run. Unknown activity is not an instruction to activate a scene.
Ordinary mixed Timed/Immediate batches validate before atomic admission;
native selection-group writes reuse the existing provider and preserve its
selection/read proof in the same atomic admission. See [the entry contract](../../docs/immediate-scene-entry.md).

Synthetic tests exercise the actual Admission/Feedback/Control, HAP
characteristic callback/event emitter and Web API handler paths. They include
actual brightness different from the scene target, ignored members, All/Any,
unknown strategies, dark combined lights, scope/readiness boundaries, read-only
write rejection, subscription invalidation/expiry/recovery and zero additional
control output. A synthetic localhost paired-controller test also verifies actual encrypted
EVENT serialization (including null invalidation and recovery) without GET
refresh. It does not establish Apple physical-client support. These are not a
field pairing or Apple Home acceptance run and
approve no original source scenes, programs or variables.
