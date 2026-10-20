# Shared logical lights

`composed_lights` adds native lamp cards over reviewed physical relay/dimmer
products. It keeps register identities, queries, module diagnostics, scene
recipes and HomeKit publication scope intact. It does not discover wiring or
turn a source inference into physical verification.

A group declares up to eight logical lights over eight relays and four dimmers.
Each light has an existing stable device identity, display name, default On
brightness and one or more reviewed supply/dimmer products. Multiple products
within one light must use one dimmer, for example a shared night-light column
feeding both bedside supplies. Physical bindings remain unique.

```json
{
  "composed_lights": [{
    "id": "example-shared-supply",
    "review_evidence": "Verified wiring and control behavior reference",
    "allow_shared_side_effects": true,
    "lights": [{
      "id": "reading-left",
      "name": "Reading left",
      "members": [{"supply": "bedside-power", "dimmer": "reading-left"}],
      "default_on_level": 100
    }, {
      "id": "reading-right",
      "name": "Reading right",
      "members": [{"supply": "bedside-power", "dimmer": "reading-right"}],
      "default_on_level": 100
    }]
  }]
}
```

The referenced devices must already be enabled physical bindings in the same
room and gateway. An existing device identity can identify a logical lamp even
when its physical register is a relay; the native view publishes it as a dimmer.
Its physical name and register type remain available through module diagnostics.
Unexposed group resources are hidden from the daily catalog. Room editing
retains the original physical definitions.

Two logical lights may share their supply or their dimmer. An identical
supply-plus-dimmer product cannot belong to two logical lights. Repeated
products, duplicate identities, unsupported profiles, cross-room references,
overlapping groups and conflicts with exclusive ordinary combined lights are
rejected by configuration validation. A composed overlay of an existing shared
matrix must cover that matrix completely.

## Control and feedback

Native power and brightness commands capture fresh feedback for the complete
group. Admission rechecks the configuration, transport generation, feedback
lease, authorization and settled prior writes before accepting the whole plan.
The original device-command credential must cover every physical dependency.
A bare physical request cannot bypass this path for a logical identity.

The planner first removes conflicting power, then writes direct brightness,
then energizes supplies. It prefers preserving other lamps and never closes a
shared resource still required by another active lamp. If isolated Off is
physically impossible, no part of the plan is admitted. The native card's action
gate reports `shared_channel_in_use`. Missing dependency feedback reports
`shared_feedback_unavailable`.

`allow_shared_side_effects` permits inherent additional activation or shared
brightness when several products use the same dimmer. It does not permit an
Off command to extinguish another active product. Optional `excludes_on`
references provide explicitly reviewed exceptions; ordinary native cards leave
that list empty. Scene exclusivity stays in the scene recipe and is not
implicitly executed by a lamp card.

Each lamp publishes the supply-and-dimmer feedback product, using the oldest
dependency timestamp and its actual freshness. Grouped lamps are On when any
member is physically powered; unknown member observations are not fabricated.
Relay or shared-dimmer feedback updates every affected native view. Pending raw
register targets cannot masquerade as confirmed logical brightness.

Restore preferences are recorded under logical identities only after their real
dependencies have settled. Two lamps sharing a dimmer retain separate powered
restore histories. On restores the last confirmed nonzero brightness, falling
back to `default_on_level`. Scene targets remain explicit: a night scene can set
10%, while other scenes that enable that fixture can set 100%; Off remains Off.

Private household wiring and candidate configurations are maintained separately
from this public schema description. Passing simulation proves the software
path, not the installation's physical wiring.
