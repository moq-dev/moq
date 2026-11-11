---
name: verify-reconnect
description: Model-check the client connect policy (`rs/moq-net/src/reconnect.rs`) and the `moq_tokio::Connection` loop that drives it with TLA+. Use when a change touches GOAWAY handovers, the predecessor's drain and retirement, the action queue, fallback probes, what a `Connection` reports (status, presence, the predecessor `close` drains), or which address a redial picks after a redirect.
---

# Verify reconnect and GOAWAY handovers

`rs/moq-net/verify/reconnect/Handover.tla` models `Reconnect` as `Connection::run` in `rs/moq-tokio/src/connection.rs` drives it. Each step is one pass of `Reconnect::poll`'s loop, ending in the action `run` applies to `Shared` in the same step, or one dial or probe finishing. A queued action after the first waits for a later poll, so a dial can finish in between, as in `run`. The peer sends a GOAWAY or closes a session at any time, and every deadline passes at any time. TLC checks every ordering of those under the bounds in the config.

## Rules

Each rule is an INVARIANT; the harm says why it matters. All but `NoOrphan` and `RedirectFollowed` are checked only while `run` waits with the queue drained, which is when another task can read the handle.

| Rule | Holds when | Harm if broken |
| --- | --- | --- |
| `NoStaleSession` | The session a `Connection` reports as live is never one the policy aborted or started closing. | An app opens subscriptions on a session that is going away. |
| `StatusTruthful` | Connected is on the policy's live session, Migrating on its predecessor while nothing else is live, Closed exactly when the policy is done, and the predecessor `Connection::close` drains is the policy's. | Status or `close` acts on the wrong session, or a closed loop looks alive. |
| `OfflineMeansNothingServes` | Disconnected is reported only when no predecessor still serves (`Action::Offline`: "nothing serves anymore"). | An app waiting on `established` stalls, and the UI says disconnected, while a session still carries its media. |
| `NoOrphan` | Until the policy is done, every open session is the live one or the predecessor. | A transport and the peer's state stay open with nothing able to reach them. |
| `PresenceBalanced` | `sessions_started - sessions_ended` is 1 while connected and 0 otherwise. | Presence counts drift on every migration. |
| `RedirectFollowed` | A dial never goes elsewhere than a redirect, named to a session the policy still holds, that the policy never acted on. | The replacement lands back on the node that is leaving. |

## Run

```sh
quest gate just rs tla moq-net/verify/reconnect/Handover.tla
quest gate just rs tla moq-net/verify/reconnect/Handover.tla -config rs/moq-net/verify/reconnect/RestartWait.cfg
quest gate just rs tla moq-net/verify/reconnect/Handover.tla -config rs/moq-net/verify/reconnect/RedirectLost.cfg
quest gate just rs tla moq-net/verify/reconnect/Handover.tla -config rs/moq-net/verify/reconnect/OfflineWhileServing.cfg
```

The first checks every rule but the two with open findings, over four sessions: about 1.2M distinct states, under a minute on 32 cores. The second checks the same rules under `Restart::Wait` over three sessions: about 240k states. Both pass at the sync commit. The last two fail at the sync commit, each on its finding below, within seconds. The `VIEW` counts states that differ only in what nothing reads again as one: a closed session's details unless it is still the live one, a deadline of a phase the policy left, and where no dial is headed.

Read a counterexample with `/statechart`, passing it the spec, a config, and `Handover.view.ts`, which labels what the handle reports, what the policy holds, and each step as the event that happens.

## Open findings

### A redirect after a probe got through is lost

`RedirectLost.cfg` fails. On a fallback, `probed` moves the live session into the predecessor and drops its `goaway::Consumer` with the `Live` it came from, so nothing reads that session's GOAWAY anymore. A redirect its peer sends while the handshake over the preferred transport runs is never followed: when that handshake fails, the next walk dials the address the peer is leaving. `residual_a_redirect_after_a_probe_got_through_is_lost` in `reconnect/tests.rs` replays it. Once fixed, flip that test, move `RedirectFollowed` into `Handover.cfg`, and delete `RedirectLost.cfg`.

### A replacement dropping mid-handover reports Disconnected

`OfflineWhileServing.cfg` fails. When the replacement closes while the predecessor still drains, `closed` queues `Action::Offline`, whose doc promises nothing serves, and `Shared::offline` reports Disconnected and stops handing out a session, though the predecessor still carries its subscriptions until it retires. `residual_a_dropped_replacement_reports_offline_while_the_predecessor_serves` replays it on the policy. Once fixed, flip that test, move `OfflineMeansNothingServes` into `Handover.cfg`, and delete `OfflineWhileServing.cfg`.

## Reading a failure

A counterexample is a path through the model, not yet a bug. Replay it on the policy with the helpers in `reconnect/tests.rs` (`next`, `handshake`, `on_fallback`, `probe`, `connected`, `failed`, `probed`, a server's `drain().send(..)`), one call per step, on the simulated clock. A path through `Shared` needs `moq-tokio`'s connection tests instead. If the code does not follow the path, the model is wrong: fix the model to match the code. If it does, report the path and the rule in the code's words. Do not weaken a rule, add a constraint, or shrink the bounds until the check passes.

## Coverage

Sync commit: `103052ed074a87f00269ecb0d639905482ecfb6f`. Run `git log 103052ed07.. -- rs/moq-net/src/reconnect.rs rs/moq-net/src/reconnect/ rs/moq-tokio/src/connection.rs` to see what changed since. After a change to a covered function, mirror it in the model, rerun every config, and move the sync commit forward only once the model describes the code at the new commit.

| Code | Model |
| --- | --- |
| `reconnect.rs` `Reconnect::poll` (queue, predecessor, phases) | `Pop`, `Retire`, `IdleClose`, `Dial`, `Abandon`, `BackoffEnds`, `LiveEnds`, `ProbeAsk`, `ProbeAbandon` |
| `reconnect.rs` `connected`, `fallback`, `failed`, `probed` | `Connected`, `Failed`, `ProbeFails`, `ProbeGetsThrough` |
| `reconnect.rs` `end`, `closed`, `done`, `Predecessor` | `EndGoaway`, `EndClosed`, `Done`, `Retire`, `IdleClose` |
| `reconnect.rs` `Restart::Wait` | `LiveEnds` with `Restart = "wait"` (`RestartWait.cfg`) |
| `reconnect/redirect.rs` `Redirect::target` | `Refused` and `Endpoints` |
| `moq-tokio/src/connection.rs` `run`, `Shared::connected`, `migrating`, `predecessor`, `offline`, `end` | `Connected`, `Apply`, `OnMigrating`, `OnRetired`, `OnOffline`, `OnDone` |

## Not covered

One configured address, so the walk across several, `ATTEMPT`, and `Verdict::Settled` and `Unusable` are left out. So are the retry window and `Failure::Exhausted`, tokens and `Verdict::Refused`, connecting once, fixed targets, and redirect-loop scoring (`Redirects`): a session's healthiness is chosen freely instead, which covers a looped one. Backoff delays and jitter are left out; every deadline can pass in any order. `Connection::close`, `Monitor`, bandwidth forwarding, and the QUIC fresh-port retry are left out, and a session's subscriptions reduce to whether the predecessor is idle. A pass covers four sessions under `Restart::Redial` and three under `Restart::Wait`, with two endpoints a GOAWAY may name and one it may not. The tests in `reconnect/tests.rs` and `moq-tokio`'s connection tests own the timing.
