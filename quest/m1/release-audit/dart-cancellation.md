# [M] Cancel pending Dart stream reads when listeners leave

## Goal

Cancelling an idle Dart announcement, grant, or request stream completes and
releases its pending operation without waiting for an unrelated native event
or closing a shared owner that remains in use.

## Plan

P2 lifecycle defect at main 1d43c096e0. `AnnounceConsumer.updates()` in
`dart/moq/lib/src/client.dart` awaits `next()` inside an async generator and
only calls native cancel/dispose in finally. Dart defers generator
cancellation through that pending Future, so the cleanup that would release
the native read cannot run. `Auth.grants()` awaits `grantChanged()` and
`MoqServer.requests()` in server.dart awaits `accept()` with the same issue.
Announcements and server requests are inherited; AUTH grants adds a new
instance in 026030acd3.

The unchanged updates() body reproduced this with a deterministic pending
native stub on Dart 3.13.0: after subscription.cancel() and an event-loop
turn, cancellation, native cancel, and native disposal were all unfinished.
Completing next() externally allowed all three to finish. This proves the
Dart control flow; the other wrappers were source-inspected, and no full
generated/native Dart integration test ran.

Provide cancellation ownership for the pending operation rather than relying
on finally after an uncancellable Future. Audit the generated future bridge
and handwritten wrappers together; dispose only the operation where auth or
listener ownership is shared. Exercise idle cancellation, cancellation
concurrent with delivery, errors, and exactly-once cleanup without sleeps.
Keep any generated-runtime fix in its generator. Report Dart/API or FFI
changes and add migration guidance if necessary; no wire change is expected.
