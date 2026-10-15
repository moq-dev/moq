# [M] Sessions closed at the drain deadline finish their cleanup

## Goal

When a drain reaches its deadline, every established session it
force-closes finishes its cleanup before the relay's `run` returns or the
`moq` CLI exits. Cleanup means its `lease.close` with the shutdown reason and
final byte totals, and the auth server's `end` event that follows. Handshakes
and admissions that never complete still cannot hold the process past the
deadline. Both `moq-relay` and the `moq` CLI's listeners behave this way.

## Plan

From a Codex P2 on #5510, declined there because the relay has the same race.
The relay's `drain` (`rs/moq-relay/src/relay.rs`) and the CLI's `drain()`
(`rs/moq-cli/src/main.rs`) both return when the observer's activity empties,
or at the drain deadline. A session still open at the deadline is
force-closed by its own driver (`GoawayTimeout`). That timer is armed only
after the GOAWAY is sent, so it fires at or after the same instant.
`moq_relay::supervise` records `lease.close` only after `drain_session`
returns. So the process can exit, or the CLI's `drive` can abort its tasks,
before a forced session records its end. The auth client posts `end` from
a background task after `close`, so exiting right after `close` can still
lose it. The CLI also fixes its own deadline before `trigger.start()`, a few
nanoseconds earlier than the observer's. Matching the deadline alone does not
close the race.

Root problem: the deadline arm treats two kinds of activity the same. Pending
handshakes and admissions may never finish, so the deadline must cut them.
An established session needs both a guaranteed close at the deadline and
time to finish its cleanup afterwards. Separate the two. After the deadline,
wait for established sessions to finish their cleanup, and do not wait for
pending handshakes. A second signal still exits at once.

Today nothing guarantees that close. The IETF session (`rs/moq-net/src/ietf/session.rs`)
writes the GOAWAY with `encode_message(..).await` before `goaway::enforce`
arms its timeout. A peer that keeps the transport alive but stops reading
the control stream can block that write forever, so the session never
reaches its force-close. Waiting on it would hang shutdown (found in this
plan's review). So the shared absolute deadline must close every remaining
established session itself, whatever its GOAWAY write is doing. Only then
can the drain safely wait for the cleanup and the auth `end`. Check the lite
GOAWAY path for the same ordering.

Recommendations:

- Prefer one drain shared by the relay and the CLI over two copies that
  drift. Keep any new `moq_relay::shutdown` export as small as the CLI needs.
- Bound the `end` delivery by the auth client's own request timeout. Add no
  drain-specific cap.
- Tests on a paused clock for both binaries' drain paths. A session that
  ignores the GOAWAY is force-closed at the deadline, and the drain returns
  only after its lease recorded `Shutdown` with its bytes. With an HTTP auth
  server, its `end` arrives. A peer whose GOAWAY write is blocked by flow
  control is still closed at the deadline and cleaned up. A handshake that
  never completes does not move
  the exit past the deadline. Keep `pending_handshake_cannot_extend_the_drain_deadline`
  and the CLI's `drain_bounds_a_pending_admission` passing.

Update `doc/bin/relay/` and `doc/bin/cli.md` if they promise when the
process exits relative to the deadline.

Public API: possibly a small shared drain helper in `moq_relay::shutdown`.
Wire: none.

## Related

- [Session registration](/quest/m1/test-flakes-2/cli-drain-register.md) - the start of the same drain: a session the client sees established is one the drain waits for
- [Handshake deadline covers admission](/quest/m1/handshake-admission-deadline.md) - bounds pending admissions, which the drain deadline must keep cutting
- [Session outcomes](/quest/m1/session-outcomes.md) - counts how sessions end, which needs this end recorded
