# Shared-light control readiness

A logical lamp can be confirmed Off as soon as its supply reports Off. That
does not prove every channel used to plan the next shared-light operation is
settled: a dimmer write in the same resource matrix may still await feedback.

The device catalog retains the real logical observation and reports
`action_gate.reason = shared_feedback_pending` while any resource in that
lamp's reviewed matrix fails the command journal's completion fence. Accepted
or writing commands block regardless of transport epoch. A terminal command
requires a newer actual observation in the current epoch; a synchronized newer
transport epoch retains the existing supersession rules. This snapshot check
uses one journal read and does not send queries or control commands.

The next catalog projection clears the pending gate when actual feedback
settles the matrix. It does not wait for the next periodic scan, assume a
successful write, or retry a rejected control. Admission still independently
validates the fresh matrix lease before accepting any new operation.

The regression covers a supply reporting Off before the shared dimmer reports
its new value: the logical observation remains Off, the control gate remains
pending, premature admission is rejected without output, and new feedback
reopens the gate without a refresh-cycle delay. Snapshot journal checks are
also compared with the existing single-device fence across all command states,
observation times and transport epochs.
