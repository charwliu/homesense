# HomeKit room placement

`GET /api/v1/homekit/placement` is an authenticated, administrator-only read of
the actual installed HAP graph. It does not query the serial gateway, allocate
new accessory identifiers, change a room or execute a device command. The
response uses `Cache-Control: no-store` and contains no PIN, pairing keys,
credentials or identity database paths.

The response contains `boot`, exact string `revision`, an opaque graph
`generation`, and `bridges`. Each bridge has `id`, `name`, `available` and
`accessories`. Each accessory has `kind` (`light`, `scene`, `curtain`), logical
`id`, `aid`, `serial_number` and optional `room_id`. Bindings come from the graph
that was actually installed, not all configured devices or a planned selection.
Unassigned room metadata remains null rather than guessing a destination.

Metadata is unavailable while management is paused, after a boot/revision
mismatch, or before a graph has been installed. A failed bridge is marked
unavailable without reallocating or discarding its public identifiers. A new
graph installation changes `generation`, even at the same revision.

Each bridged accessory also publishes `HomeSense-AID:<aid>` in its public model
information. Its AID, IID and `HS-<aid>` serial remain unchanged. The iOS client
uses `HMAccessory.manufacturer` and `model` to derive the same scoped identity;
it does not depend on the unsupported serial-number characteristic. If vendor
access provides a native HAP instance ID, it must agree with an explicit model
ID. A plain old `HomeSense` model without a vendor ID is not a valid match.

The App requests this publication explicitly through its authenticated server
connection. It rejects metadata from another snapshot boot or revision and does
not poll or retry writes. Its pure placement planner requires the user to bind
each server bridge to a native HomeKit bridge and each room to a native room ID
or an explicit new-room name. Serial numbers are scoped to that selected
bridge's children: two bridges can legitimately both contain `HS-2`.

On iPhone/iPad, an administrator opens **Service connection → Apple Home room
placement**, explicitly requests HomeKit access, chooses the home and binds
each server bridge to a native bridge. Unique room-name matches are suggested;
missing rooms may be created as part of the reviewed batch. Unassigned/global
accessories need an explicit existing destination room. No HomeKit manager is
created at ordinary App startup.

Previews reject ambiguous identifiers, changed native bridge membership,
manual room moves, changed publication generations and conflicting new-room
names. A caller must rediscover and create a new preview after partial changes;
it must not reuse the old preflight baseline after its own successful moves.
The executor verifies its own completed moves, rechecks the current publication
before each mutation, stops on failure or cancellation, and never automatically
replays an unknown result. A fresh preview skips accessories already placed.
The native adapter keeps an outstanding SDK-write fence until its actual
callback, even when the UI has cancelled or its wait has timed out; closing and
reopening the placement sheet does not clear that fence.

Native authorization/discovery, the room selection UI and assignment executor
are implemented in the iOS adapter. Final completion still requires a valid
HomeKit-capable signed build, system authorization, and actual iPhone/iPad
room-assignment verification. A second newly published bridge must be paired
before its accessories can be selected; the original bridge is not re-paired.
