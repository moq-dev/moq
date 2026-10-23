# [S] Go cancelled results

## Goal

A Go call whose context is cancelled just as the native call completes
destroys the handle it produced right away, instead of leaving it live until
the garbage collector runs its finalizer. The case that matters is
`Server.Accept`: today the incoming session it accepted sits unanswered until
GC, and the same goes for subscriptions, fetches, resolves, and broadcast
requests the caller gave up on.

## Plan

Found in the m1 simplification audit (2026-10-23) and checked against the
generated binding. The cause is in `uniffiRustCallAsync`, which
`kixelated/uniffi-bindgen-go` generates. When ctx is cancelled, it cancels
the Rust future and waits for it. If the future had already completed, the
result is lifted into a Go object anyway, then dropped behind
`ctx.Err()`. The object has a finalizer, so it is reclaimed eventually but
not promptly.

The wrapper cannot close this. `bridge.CallHandle` releases a late handle
only when the call has its own `cancel` (as `Request.Accept` does), because
it then gives the binding a context that never cancels. These calls have no
such `cancel`, so the Rust future would keep running after the caller
leaves. For `Server.Accept` that future would take the next incoming
request, which the release then destroys: a real connection lost. With a nil
`cancel`, `CallHandle` passes ctx through, so `Auth.Add` leaks the same way.

Fix it in the generator: when the call was cancelled, run the result type's
destroyer instead of returning nothing. The generator already emits a
destroyer per type, including records and optionals that hold objects. Then
the wrapper's direct ctx calls (`Server.Accept`, `SubscribeTrack`,
`FetchGroup`, `Resolve`, `RequestBroadcast`) and `Auth.Add` are correct as
they are. A binding that destroys the result returns `ctx.Err()`, which
`run` never releases, so nothing is destroyed twice. Check
whether `bridge.CallHandle` still covers anything once the binding releases
these results itself. If the
[generator hard fork](/quest/m2/uniffi-fork.md) has landed, the change goes
there; otherwise into the current fork, with a pin bump.

Test it with a cancelled context racing a call that completes at once, and
count live native handles rather than waiting on GC.
