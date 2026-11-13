# More tests hold up under load

## Goal

A second round after [#4286](https://github.com/moq-dev/moq/pull/4286):
tests that pass alone but have failed under a loaded `just check`, or
intermittently in CI or interop, pass reliably, each fixed at its cause,
never by raising a timeout or adding a retry.

## Plan

Decided in the 2026-09-30 audit: the round held independent flakes in one
quest, so it split into one child per flake, grouping only those that share
a fixture. Each child lands on its own.

Rules every child keeps:

- Prefer a paused clock over wall time (`moq-cli`'s subscribe tests already
  use `#[tokio::test(start_paused = true)]`), or assert on an event instead
  of a deadline. If a test is slow under load because the code under test is
  slow, fix that.
- A paused clock auto-advances while the runtime idles, so it can fire a
  timer before a real socket delivers. Where real sockets fight the paused
  clock, drive the timers with a `kio::clock::Manual` clock or the sim (every
  crate's timers run on `kio::clock`), or move the test off real sockets.
- A mocked clock still flakes when the code jitters its timers with `rand`
  (one cancel race was a retry sweep jittered past a fixed settle
  budget). Wait out the jitter's whole window, not a count of small steps.

This README's own work, after the children: run `just check --all` several
times on a loaded machine, as the first round did.

The redundant failover drift moved to m0 on 2026-10-20 (maintainer) and is
fixed by [moq-dev/moq#6113](https://github.com/moq-dev/moq/pull/6113).

Added 2026-10-21 and 2026-10-22 (maintainer): failures from the m0/m1
merge runs, one child each.

Decided 2026-10-26 (maintainer): the epic moved from m1 to m0, since every
flake drops a queued PR from the merge queue (it then failed a whole batch of
the [batched merge gate](https://github.com/moq-dev/moq/blob/17a81d6c64/quest/m0/ci/land.md),
dropped 2026-11-01). The queue's recurring flakes from 2026-10-23 to 10-26
were already fixed (the moq-rtsp stall tests, the moq-gst pinned-instance and
sibling-restart tests, the moq-mux missing-sequence test,
`js/common/worklet.test.ts`, and the IETF datagram range test in
[moq-dev/moq#6593](https://github.com/moq-dev/moq/pull/6593)).

The `rust-swapped -> js` and `rust -> js` browser audio timeouts were a lite-06
catalog group dropped mid-read ending the player's catalog, fixed by
[moq-dev/moq#6820](https://github.com/moq-dev/moq/pull/6820).

moq-webrtc's session-end test was never load-bound: on the paused clock, dimpl's
randomly jittered DTLS handshake give-up raced ICE's establishment deadline,
fixed by [moq-dev/moq#7379](https://github.com/moq-dev/moq/pull/7379).

moq-relay's auth-trust test was never load-bound either: its fixture left each
lease's background `end` POST unserved, so a reused port let one steal the next
trial's accept. Fixed by [moq-dev/moq#7550](https://github.com/moq-dev/moq/pull/7550).

The embedded relay stop test probed freed ports after its Linux socket-inode
check had already proved the owner's HTTP and QUIC sockets closed. A successful
TCP connect could reach a replacement listener. Linux now uses the ownership
proof alone, fixed by [moq-dev/moq#7823](https://github.com/moq-dev/moq/pull/7823).

The Kotlin flow-cancel test's `unroutable` was not load-bound: its snapshot
writer became unreachable, so JVM cleanup aborted the track mid-read. The JS
IETF blind-handle test's one identified cause was the session-closing reset
fixed by [moq-dev/moq#6988](https://github.com/moq-dev/moq/pull/6988). A single
event-loop stall at each of its 124 host tasks passes all 124 at 1.5 seconds;
at 4.5 seconds 3 runs and at 11 seconds 114 runs hit bun's own 5-second test
timeout. No run fails on a protocol deadline, so no clock change applies. Both handled in [moq-dev/moq#7622](https://github.com/moq-dev/moq/pull/7622).

The JS announce harness assumed one host turn drained all request writes. It
now waits until the subscriber reads the response, with forced write-budget
yields covering lite-03 and lite-04, fixed by [moq-dev/moq#7663](https://github.com/moq-dev/moq/pull/7663).

The JS close-drain fixture advanced after the setup group's first payload, so a
zero-delay subscription could expire its sender while the write budget yielded.
It now reads that group through FIN before starting the next one, fixed by
[moq-dev/moq#7658](https://github.com/moq-dev/moq/pull/7658). The lite-04 case
of the same test failed once more in the batch for
[moq-dev/moq#7431](https://github.com/moq-dev/moq/pull/7431)
([run 41573678257](https://github.com/moq-dev/moq/actions/runs/41573678257),
2026-11-03 03:45Z) with `Session: remote error: 0`, the cancellation that PR
explains. That batch predates #7658's merge at 15:56Z, and no queue run since
has failed it, so it needs no child of its own (noted 2026-11-03,
`/quest-plan`).

The JS reload close-drain test lost a race in the lite-07 publisher: the
subscriber's FIN after reading the tail could land while a reply write, such as
SUBSCRIBE_FINISH, still yielded on the write budget. That exit stopped owing the
opened group streams, so a draining close aborted a final group whose FIN was
still in flight. Every exit now owes them, fixed by
[moq-dev/moq#8047](https://github.com/moq-dev/moq/pull/8047).

Added 2026-11-03 (maintainer, `/quest-plan`): five more merge-queue failures
from 2026-11-03 and early 2026-11-04 UTC, one child each.

Added 2026-11-04 (maintainer, `/quest-plan`): moq-gst's interleaved restart
test, which failed two merge-queue batches and two loaded local runs.

Added 2026-11-05 (maintainer, `/quest-audit`): Interop's P2P data-channel
timeout, one child.

Added 2026-11-06: `js/net`'s lite-06 datagram end/close case, from `Check` on
[moq-dev/moq#7981](https://github.com/moq-dev/moq/pull/7981).

Added 2026-11-05 (maintainer, `/quest-complete` follow-ups): moq-net's
pool-debt flight test, the Python audio playout-delay test, and the io_uring
tests' shared locked-memory budget, one child each. The pool-debt test
overshot because shed frames whose payload canonical content owned stayed
billed until crossbeam-epoch reclamation, which other tests' reads in the same
process held back; eviction now lets go of that content itself, fixed by
[moq-dev/moq#8052](https://github.com/moq-dev/moq/pull/8052).

moq-ffi's concurrent-ready test drained its reader until the group ended, which
an unfinished group never does. Its `ffi_caught_up` returned without yielding
whenever the FFI runtime answered first, so under load the woken `ready` calls
missed their turn until every frame was read. Fixed by reading at most what was
written and yielding after each read, in
[moq-dev/moq#8053](https://github.com/moq-dev/moq/pull/8053).

The Python audio playout-delay test was never a decoder stall: under its default
zero max delay, a read more than the 80 ms warmup late found the first frame's
group stale, and the warmup then trimmed all the audio left. The test now asks
for no max delay, fixed by [moq-dev/moq#8050](https://github.com/moq-dev/moq/pull/8050).

Added 2026-11-06: `js/net`'s flate tail-shed test, which timed out in a
merge-queue batch. It was CPU-bound, not racy: 100 one-MiB frames of pako
deflate and inflate against a 32 MiB tail budget that sheds from the 33rd. It
now writes four frames past the budget, fixed by
[moq-dev/moq#8137](https://github.com/moq-dev/moq/pull/8137).

Added 2026-11-06 (maintainer's standing rule that flakes join this epic):
`js/net`'s datagram-end close case, which failed
[moq-dev/moq#7986](https://github.com/moq-dev/moq/pull/7986)'s Check while
reproducing on unchanged `main`. Its end waited a host task when the
publisher's write budget yielded under load while writing SUBSCRIBE_FINISH, past
the case's microtask drain. [moq-dev/moq#8000](https://github.com/moq-dev/moq/pull/8000)
stubbed that budget, merging after the quarantine was written. The case was
re-enabled in [moq-dev/moq#8110](https://github.com/moq-dev/moq/pull/8110).
Its follow-up: a peer's protocol violation, such as a duplicate SETUP, fails the
session without the drain a close gets, so a lite-05/06 datagram track whose
SUBSCRIBE_FINISH arrived before its FIN aborted with the session's error. Its
datagrams now finish whenever the stream track's settled end survives the abort,
fixed by [moq-dev/moq#8157](https://github.com/moq-dev/moq/pull/8157).

moq-relay's `embed::worker_tokio_custom_route_and_quic` was never load-bound
either. Its socket-ownership check matched the HTTP and QUIC ports without
their protocol, so the test's own QUIC client, still draining, counted as the
relay's whenever its UDP port shared the HTTP listener's TCP port number.
Fixed by [moq-dev/moq#8236](https://github.com/moq-dev/moq/pull/8236).

`sh/dispatch-query.test.py`'s terminate test was never load-bound. A TERM
landing just after the dispatcher's `wait -n` returned left Bash marking the
exit trap's next `wait` as interrupted, so it returned 143 at once and errexit
skipped the timing report. A TERM landing after a lane forked but before
`running` recorded it left that lane uncancelled and the exit trap's `wait`
hung. The exit trap now waits again, and cancellation reads the job table,
fixed by [moq-dev/moq#8138](https://github.com/moq-dev/moq/pull/8138).

moq-gst's sink-restart pad test caught a real source bug: a track that failed on
an instance still up, such as a relay's copy cut off as its publisher's session
closed, left no finished record, so the final catalog snapshot started the
rendition again and replayed a group on its held pad. A failure now counts as
the track's end, fixed by [moq-dev/moq#8160](https://github.com/moq-dev/moq/pull/8160).

moq-gst's stale-completion sink test, which failed once in 30 stress iterations
in [moq-dev/moq#8160](https://github.com/moq-dev/moq/pull/8160)'s validation,
because the sink reported a stopped session as a terminal failure; the session
now tells a stop from a give-up through `Connection::closed`, fixed by
[moq-dev/moq#8246](https://github.com/moq-dev/moq/pull/8246).

`js/net`'s lite-07 relative-start integration test, which failed once on main in
[moq-dev/moq#8165](https://github.com/moq-dev/moq/pull/8165)'s baseline run,
read a start of 0 when the newest group landed before the reader asked: its
window had yet to settle the relative anchor, so it asked a fill from group 0.
`start()` now settles the window first, as the Rust `poll_start` does, fixed by
[moq-dev/moq#8197](https://github.com/moq-dev/moq/pull/8197).

Added 2026-11-07 (maintainer, /quest-complete): `js/net`'s
http:// fingerprint tests, whose slow real fetch lets the WebTransport hedge
timer fire on the host clock, seen in
[moq-dev/moq#8163](https://github.com/moq-dev/moq/pull/8163)'s CI.

Added 2026-11-07 (maintainer's standing rule that flakes join this epic):
`js/net`'s lite-07 empty-window reply test, which fails on main 12/200 under
`--rerun-each 200`, found in [moq-dev/moq#7670](https://github.com/moq-dev/moq/pull/7670).
A subscriber FIN during the reply write's budget yield resets the stream. It is
quarantined with `test.skip` until its child fixes it.

Added 2026-11-07 (maintainer, /quest-complete): moq-relay's
`socket_auth::a_forged_dial_never_connects`, which failed
[moq-dev/moq#8204](https://github.com/moq-dev/moq/pull/8204)'s Check when
mallory's request for bob's endpoint synced before his announce arrived. It is
quarantined with `#[ignore]` until its child fixes it.

Added 2026-11-07 (maintainer, /quest-complete): moq-uring's qlog accounting
test, which spins a fixed count waiting on a background thread, from
[moq-dev/moq#8208](https://github.com/moq-dev/moq/pull/8208)'s follow-ups.

Added 2026-11-08 (maintainer): `js/net`'s bandwidth sampling test and lite-07
createWritable datagram queue test, which failed under load. So is the lite-06 case of the
datagram session-close-after-end test, which dropped
[moq-dev/moq#8280](https://github.com/moq-dev/moq/pull/8280) from the merge queue. All are
quarantined with `test.skip` until their children fix them.

Added 2026-11-08 (maintainer, /quest-complete): `js/net`'s widened max-age
re-drop test, which failed on [moq-dev/moq#8230](https://github.com/moq-dev/moq/pull/8230)
with the `Cancel` reset [moq-dev/moq#8195](https://github.com/moq-dev/moq/pull/8195) intends for a reply still pending at the stream's end; the test races its finish against that reply.

Added 2026-11-08 (maintainer's standing rule that flakes join this epic):
`bench/relay.test.py`'s stream-transport test, which binds UDP on the number the
OS picked for TCP and failed a local run while other checks held that UDP port.

Added 2026-11-08 (maintainer's standing rule that flakes join this epic): moq-ffi's
raw media warmup test, which failed in the merge queue for
[moq-dev/moq#8272](https://github.com/moq-dev/moq/pull/8272) when its reader skipped one of three
buffered groups on the FFI runtime's real clock.
[moq-dev/moq#8329](https://github.com/moq-dev/moq/pull/8329) moved it to the paused test
clock, re-enabled it, and deleted its child.

Added 2026-11-09 (maintainer, /quest-complete): `js/net`'s compressed-frame
cap test and lite-07 re-dropped re-offer test, which failed under load during
the datagram-queues runs and were quarantined with `test.skip` by
[moq-dev/moq#8333](https://github.com/moq-dev/moq/pull/8333); and moq-net's
two random-mesh withdrawal tests, which hit nextest's 60 s kill in
[moq-dev/moq#8247](https://github.com/moq-dev/moq/pull/8247)'s local
`just check` and are quarantined with `#[ignore]` by
[moq-dev/moq#8336](https://github.com/moq-dev/moq/pull/8336).

Added 2026-11-09 (maintainer's standing rule that flakes join this epic):
`js/net`'s datagram session-close-after-end test again, whose lite-05 case
failed the merge queue for [moq-dev/moq#8276](https://github.com/moq-dev/moq/pull/8276)
after [moq-dev/moq#8321](https://github.com/moq-dev/moq/pull/8321) re-enabled it.
The test now aborts once the client has recorded the end, signalled by the
subscriber's `yieldNow()`, and is re-enabled by
[moq-dev/moq#8357](https://github.com/moq-dev/moq/pull/8357).

Added 2026-11-09 (maintainer's standing rule that flakes join this epic):
moq-rs's Python socket test `test_tcp_rst_and_endpoint_close_are_errors`, whose
reader got `Unroutable` after the endpoint closed during
[moq-dev/moq#8293](https://github.com/moq-dev/moq/pull/8293)'s loaded `just check`.
It is quarantined with `pytest.mark.skip` until its child fixes it.

Added 2026-11-09 (maintainer's standing rule that flakes join this epic): the
Go wrapper's `TestRecvGroupCancelKeepsConsumer`, whose `RecvGroup` after a
cancelled one missed an appended group during
[moq-dev/moq#8363](https://github.com/moq-dev/moq/pull/8363)'s loaded
`just check`. It is quarantined with `t.Skip` until its child fixes it.

Added 2026-11-09 (maintainer, `/quest-complete`, under the flaky-test rule):
`js/common/worklet.test.ts`'s dev-server test, a candidate for bun's 5 s timeout
in a loaded full `just check` on [moq-dev/moq#8325](https://github.com/moq-dev/moq/pull/8325).
The failed log was not retained, so the failing test is unconfirmed.
It is quarantined with `test.skip` until its child fixes it.

Added 2026-11-09 (maintainer, /quest-complete): interop's `finish` cases with
a cpp or rust publisher, which failed under a loaded host and passed on rerun.
They are quarantined until their child fixes them.

Resolved 2026-11-09 (maintainer, `/quest-complete`): `js/net`'s lite-07
createWritable datagram queue test failed only on code predating [moq-dev/moq#8195](https://github.com/moq-dev/moq/pull/8195): a write
that yielded after handing DATAGRAMS_INFO over left the first track's queue
unopened while the reader went on. The test now reads a datagram through that
queue before counting queues, fixed by
[moq-dev/moq#8396](https://github.com/moq-dev/moq/pull/8396).

Added 2026-11-10 (maintainer's standing rule that flakes join this epic):
interop's `python -> go` media cell, whose Go subscriber closed its lite-07
session as publishing outside its grant once in
[moq-dev/moq#8444](https://github.com/moq-dev/moq/pull/8444)'s local run and
passed on rerun. It is quarantined until its child fixes it.

Added 2026-11-12 (maintainer's standing rule that flakes join this epic):
moq-gst's moqsink restart pad test, which failed the merge queue for
[moq-dev/moq#8551](https://github.com/moq-dev/moq/pull/8551) on unchanged main,
its third failure since 2026-10-16 and its first after
[moq-dev/moq#8160](https://github.com/moq-dev/moq/pull/8160) and
[moq-dev/moq#8526](https://github.com/moq-dev/moq/pull/8526). It is
quarantined with `#[ignore]` until its child fixes it.

The `go -> python` media cell failed with `audio gap after 648 samples` in
scheduled run [43260674067](https://github.com/moq-dev/moq/actions/runs/43260674067).
[moq-dev/moq#8633](https://github.com/moq-dev/moq/pull/8633) investigated
[moq-dev/moq#8531](https://github.com/moq-dev/moq/pull/8531) and ran ten complete
full matrices in the dev shell with retained debug logs. The cell passed every
one; nine matrices passed entirely. The repeated-run outcome, authorized by the
maintainer on 2026-11-13, retires its separate quest without claiming a runtime
fix or ruling out a rarer failure. The shared container's CPU-starvation skip
is being fixed in [moq-dev/moq#8611](https://github.com/moq-dev/moq/pull/8611).

Public API: none. Wire: none.
