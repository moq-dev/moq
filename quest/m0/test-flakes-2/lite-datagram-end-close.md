# [S] A close after a datagram track's end keeps the end under load

## Goal

`a session close right after a datagram track's end keeps the end` in
`js/net/src/lite/datagram-tracks.test.ts` passes reliably in loaded runs, fixed
at its cause under the [epic](/quest/m0/test-flakes-2/README.md)'s rules, and
its `test.skip` is removed. Done when the PR explains the race and shows the
loaded run that reproduced it now passing.

## Plan

Added 2026-11-06 (maintainer's standing rule that flakes join this epic, CI fix
run on [moq-dev/moq#8007](https://github.com/moq-dev/moq/pull/8007)). Check
`test rs`/`test js` failed the lite-06 case once, and the PR does not touch the
test or the datagram paths. Quarantined with every assertion kept, since the
same test fails on unchanged `main`.

Reproduced on both trees by running the file beside one busy loop per core
(4 cores): `main` failed 4 of 8 runs, the PR head 7 of 8, always the lite-06
case, and always as `Session: remote error: 0` from `recv()`. Instrumenting
`Subscriber.close` showed the datagram entry's `ended` was still unset when the
close ran: the subscriber had not read the DATAGRAMS_INFO FIN within the test's
`for (let i = 0; i < 1000; i++) await Promise.resolve()` drain. The test comment
assumes the mock delivers the end in microtasks within the same host task, but
under load part of the publisher's `track.recv() -> sender.close() ->
stream.writer.close()` chain, or the subscriber's `opened.reader.done()`, needs
a host task, so the abort wins.

`Subscriber.close` already finishes a datagram producer whose track `ended` and
only aborts the rest, and Rust's `end_datagrams` moves an ended track out of the
abortable map. So the intended behavior is clear; the test's fixed microtask
drain is not a reliable way to observe that the FIN arrived. Lead, unverified:
wait for an observable of the receipt (for example a package-internal hook set
where `route.ended = true` runs, like the existing `hooks`), then close while the
end is still deferred by `#datagramsRouted`'s `yieldNow()`, which is the window
the test means to cover. Do not raise the microtask count or add a retry.

Reproduce first: run `js/net/src/lite/datagram-tracks.test.ts` beside one busy
loop per core and keep a failing run's output.

Public API: none. Wire: none.

## Waiting

- moq-dev/moq: capture a failing loaded run of the file with logs before
  choosing the observable to wait on, then remove the skip.
