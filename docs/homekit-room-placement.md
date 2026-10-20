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

The App requests this publication explicitly through its authenticated server
connection. It rejects metadata from another snapshot boot or revision and does
not poll or retry writes. Its pure placement planner requires the user to bind
each server bridge to a native HomeKit bridge and each room to a native room ID
or an explicit new-room name. Serial numbers are scoped to that selected
bridge's children: two bridges can legitimately both contain `HS-2`.

Previews reject ambiguous identifiers, changed native bridge membership,
manual room moves, changed publication generations and conflicting new-room
names. A caller must rediscover and create a new preview after partial changes;
it must not reuse the old preflight baseline after its own successful moves.

This read API and planner do not by themselves perform native HomeKit room
assignment. Native discovery/authorization, the assignment UI and executor,
partial-failure reporting, and real iPhone/iPad verification remain required.
