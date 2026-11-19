---
name: verify-reconnect
description: Model-check the client connect policy (`rs/moq-net/src/reconnect.rs`) and the `moq_tokio::Connection` loop that drives it with TLA+. Use when a change touches GOAWAY handovers, the predecessor's drain and retirement, the action queue, fallback probes, what a `Connection` reports (status, presence, the predecessor `close` drains), or which address a redial picks after a redirect.
---

# Verify reconnect and GOAWAY handovers

`rs/moq-net/verify/reconnect/Handover.tla` models `Reconnect` as `Connection::run` in `rs/moq-tokio/src/connection.rs` drives it. Each step is one pass of `Reconnect::poll`'s loop, ending in the action `run` applies to `Shared` in the same step, or one dial or probe finishing. A queued action after the first waits for a later poll, so a dial can finish in between, as in `run`. The peer sends a GOAWAY or closes a session at any time, and every deadline passes at any time. TLC checks every ordering of those under the bounds in the config.

## Rules

Each rule is an INVARIANT; the harm says why it matters. All but `NoOrphan`, `RedirectFollowed`, and `FirstWalkFailsFast` are checked only while `run` waits with the queue drained, which is when another task can read the handle.

| Rule | Holds when | Harm if broken |
| --- | --- | --- |
| `NoStaleSession` | The session a `Connection` reports as live is never one the policy aborted or started closing. | An app opens subscriptions on a session that is going away. |
| `StatusTruthful` | Connected is on the policy's live session, Migrating on its predecessor while nothing else is live, Closed exactly when the policy is done, and the predecessor `Connection::close` drains is the policy's. | Status or `close` acts on the wrong session, or a closed loop looks alive. |
| `ConnectedMeansEstablished` | A `Connection` reports a session only once the policy reported it established (`Action::Established`): its peer accepted the credential or drained it with a GOAWAY, or, on a version that never answers the credential (moq-lite 03/04), it stayed up past the healthy bar. Uptime alone never establishes a session on a version that answers. | A refused session reads as connected, and `established()` returns `Ok` for it, or a lite-07 session the peer never accepted reads as connected once it is a second old. |
| `OfflineMeansNothingServes` | Disconnected is reported only when no predecessor still serves (`Action::Offline`: "nothing serves anymore"); one closing gracefully carries nothing and takes no new work. | An app waiting on `established` stalls, and the UI says disconnected, while a session still carries its media. |
| `NoOrphan` | Until the policy is done, every open session is the live one or the predecessor. | A transport and the peer's state stay open with nothing able to reach them. |
| `PresenceBalanced` | `sessions_started - sessions_ended` is 1 while connected and 0 otherwise. | Presence counts drift on every migration. |
| `FirstWalkFailsFast` | Until a session was reported established (see `ConnectedMeansEstablished`; a GOAWAY counts, since no refusal sends one), no failed dial or closed session backs off: either ends the policy. Only a session reported established establishes the policy. | A setup mistake (an untrusted certificate, no version in common) or a refused credential retries forever instead of surfacing. |
| `RedirectFollowed` | A dial never goes elsewhere than a redirect, named to a session the policy still holds, that the policy never acted on. | The replacement lands back on the node that is leaving. |

## Run

```sh
quest gate just rs tla moq-net/verify/reconnect/Handover.tla
quest gate just rs tla moq-net/verify/reconnect/Handover.tla -config rs/moq-net/verify/reconnect/RestartHandoff.cfg
```

The first checks every rule over four sessions: about 19M distinct states, about four minutes. The second checks the same rules under `Restart::Handoff` over three sessions: about 3.4M states, under a minute. Each session picks whether its version answers the credential. All pass at the sync commit. Nightly runs every config here and fails if a config with an `\* Open finding:` line passes. The `VIEW` counts states that differ only in what nothing reads again as one: a closed session's details unless it is still the live one, a deadline of a phase the policy left, and where no dial is headed.

## Open findings

None.

## Reading a failure

Follow Reading a failure in `rs/moq-net/verify/README.md`. Replay the path on the policy with the helpers in `reconnect/tests.rs` (`next`, `handshake`, `on_fallback`, `probe`, `connected`, `failed`, `probed`, a server's `goaway(..)`), one call per step, on the simulated clock. A path through `Shared` needs `moq-tokio`'s connection tests instead.

## Coverage

Sync commit: `51e8538a84adacd16f91ef9ef6d54acfab7289db`. Run `git log 51e8538a84.. -- rs/moq-net/src/reconnect.rs rs/moq-net/src/reconnect/ rs/moq-tokio/src/connection.rs` to see what changed since. Follow Coverage in `rs/moq-net/verify/README.md` after a change to a covered function.

| Code | Model |
| --- | --- |
| `rs/moq-net/src/reconnect.rs` `Reconnect::poll` (queue, predecessor, phases), `follow` | `Pop`, `Retire`, `IdleClose`, `FollowPred`, `Dial`, `Abandon`, `BackoffEnds`, `Establish`, `LiveEnds`, `ProbeAsk`, `ProbeAbandon` |
| `rs/moq-net/src/reconnect.rs` `established`, set in `poll` by a GOAWAY, the session's acceptance, or on a silent version the healthy bar, and `walk_failed` and `closed` failing fast until it is set | `established`, the session's `accepted`, the session's `silent` (`Session::is_silent`), `Healthy`, `Ages`, `EndGoaway`, `LiveEnds`, `Failed`, `Abandon`, `EndClosed` |
| `rs/moq-net/src/reconnect.rs` `connected`, `fallback`, `failed`, `probed` | `Connected`, `Failed`, `ProbeFails`, `ProbeGetsThrough` |
| `rs/moq-net/src/reconnect.rs` `end`, `closed`, `done`, `Predecessor` | `EndGoaway`, `EndClosed`, `Done`, `Retire`, `IdleClose` |
| `rs/moq-net/src/reconnect.rs` `Restart::Handoff` | `LiveEnds` and `WaitOut` with `Restart = "handoff"` (`RestartHandoff.cfg`) |
| `rs/moq-net/src/reconnect/redirect.rs` `Redirect::target` | `Refused` and `Endpoints` |
| `rs/moq-tokio/src/connection.rs` `run`, `Shared::connected`, `migrating`, `retired`, `offline`, `end` | `Connected`, `Apply`, `OnEstablished`, `OnMigrating`, `OnRetired`, `OnOffline`, `OnDone` |

## Not covered

One configured address, so the walk across several, `ATTEMPT`, and `Verdict::Settled` and `Unusable` are left out. So are the retry window and `Failure::Exhausted`, tokens, connecting once, fixed targets, and redirect-loop scoring (`Redirects`): a session's healthiness is chosen freely instead, which covers a looped one. Backoff delays and jitter are left out; every deadline can pass in any order. `Connection::close`, `Monitor`, bandwidth forwarding, and the QUIC fresh-port retry are left out, and a session's subscriptions reduce to whether the predecessor is idle. A pass covers four sessions under `Restart::Redial` and three under `Restart::Handoff`, with two endpoints a GOAWAY may name and one it may not. The tests in `reconnect/tests.rs` and `moq-tokio`'s connection tests own the timing.
