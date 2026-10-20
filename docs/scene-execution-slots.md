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
