# Scene execution slots

The scheduler reuses a named timeline's execution slot when a new admission
needs space and the old terminal result is durably settled and delivered.
Otherwise, completed manual timelines accumulate until a persistent service
rejects new scenes. Recent terminal status remains queryable until space is
actually needed; the scheduler does not eagerly remove every completed run.

Retirement removes only the volatile execution object. The durable scene run,
command receipts, cancellation records and result identity remain available
for historical queries. Active timelines, pending physical writes and undelivered
results still consume bounded capacity. The concurrent capacity is unchanged.

Both capacity reservation and direct starts can reclaim delivered terminals.
They leave the cache untouched if the requested capacity still cannot fit.
The causal router's existing ownership and result-delivery contract, and the
explicit `take_results` and `retire` APIs, are unchanged. An explicitly retired
but not yet delivered result continues to consume its bounded result slot.

The regressions exercise eight ordinary manual runs with capacity two while
preserving all eight durable histories, reject new reservations while two
timelines remain pending, and deliver an explicitly retired result exactly once.
They also protect undelivered terminals and preserve cached status when a
reservation cannot succeed even after reclaiming completed work.

## Batch controls followed by module queries

`serve-automation-tcp --scene-write-mode batch-query` sends each evaluated
immediate lighting phase through the existing bounded transport batch writer.
The default `sequential` mode remains available for installations whose relay
modules still emit unsolicited control replies. Before enabling `batch-query`,
commission the verified R818 PRO modules with active feedback disabled and read
back their C7 setting. The C8 parameter is profile-specific: do not send it to
AD8 dimmers or six-key panels.

A TCP write contains at most 32 five-byte frames from the same admitted phase
and gateway. It preserves the recipe's supply/dimming order, durable receipts,
authorization and causal boundaries. Partial writes keep complete frames Written,
partial frames Uncertain and unsent frames Failed; they are never replayed.
The original two-second write deadline and 15-second feedback budget remain.

New background reads on that bus wait until the admitted lighting controls are
written, including all chunks of a larger phase. Already-owned reads finish
normally and independent buses continue. R818 PRO is then queried per channel;
AD8 supports a whole-module eight-channel query. Actual post-write replies update
the shared state and release linked feedback LEDs; sending a control frame never
counts as confirmed output. Motor STOPs, parameter operations, explicit queries,
timed programs and household operations keep their existing owners and barriers.
The scene read API retains its 500 ms bound, and App subscribers read the shared
state without triggering separate physical scans.
