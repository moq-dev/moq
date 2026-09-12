# [M] Make JSON and binary wrapper ownership releasable

## Goal

A caller can stop an owned JSON/binary reader, including a pending read, and
release its subscription. Exiting its async iterator does not leave demand
alive. Producers can distinguish clean completion from caller-requested failure.

## Plan

At dev `e2350b39a`, all five JS JSON/binary consumers keep a private `#track`
and expose no close/disposal operation. Their async iterators have no cleanup:
`js/json/src/{snapshot,stream,window}/consumer.ts` and
`js/binary/src/{snapshot,stream}/consumer.ts`. For example, JSON stream's
constructor is at `:50` and its iterator at `:135`. The underlying track keeps
each sink until its closed signal fires (`js/net/src/track.ts:493-525`).
Breaking a `for await` therefore leaves a subscription with no accessible
cleanup handle if the caller passed a temporary subscriber.

Decided: follow net's `close(error?)` shape,
close the owned track/current group, and use iterator `finally` cleanup.
Retain producer `finish` as successful completion and allow explicit failure
through the chosen close contract. Specify idempotence, pending-read wakeup,
and post-close behavior. Do not close an unrelated publisher or sibling reader.

Test normal completion, early break, exception in the loop body, explicit
close during a pending read, repeat close, and error propagation in existing
JSON/binary suites. Assert underlying demand disappears without manually
retaining and closing a separate net handle. Be explicit that returning an
iterator while its `next()` is pending needs a callable cancellation path.

Public API: adds lifecycle methods and changes early-iterator-exit behavior.
Wire: subscription teardown only, no format change. Run JS check/test.
