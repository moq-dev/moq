# Transport failure drills

Media flowing on a healthy loopback proves delivery and nothing else. These
drills disrupt a live session through a real relay over real QUIC and check what
happens next: cancellation, a relay dying mid-group, and a publisher coming back
under a name it already used.

The drills themselves are ordinary Rust integration tests in
`rs/moq-relay/tests/drills.rs`, so `just test` already runs them whenever
moq-relay or anything under it changes. This directory holds the focused recipe
and the sensitivity proof.

```bash
just test drill                 # the drills, plus the negative control
just test drill relay_killed    # one of them
just test drill-sensitivity     # prove each drill fails without its fix
```

## What each drill does

Every drill records that its fault actually activated, requires the failure to
surface as a terminal result rather than a clean finish or a hang, and then
checks that what the fault owned was released.

| Drill | Fault | Recorded activation | Asserted |
|---|---|---|---|
| `cancel_under_backpressure_releases_the_reader` | A subscriber stops reading while the publisher runs ahead, then cancels | the number of unread groups queued behind the reader at the moment it cancels | everything the cancelled session was feeding closes rather than parking, and the relay still serves a subscriber that joins afterwards |
| `relay_killed_mid_group_aborts_then_resumes` | The relay's runtime is dropped mid-group: every task and socket at once, no `CONNECTION_CLOSE` | the error the interrupted group aborted with, and both reconnect loops reporting `Disconnected` | the interrupted group fails rather than reporting a clean finish, and after the relay returns the same handles resume delivery |
| `interrupted_publisher_republishes_new_content` | A publisher vanishes with no finish and no unannounce, then a new one publishes the same name | the withdrawal of the dead publisher's broadcast | the name stops being announced, and the republished name serves the new publisher's content rather than the dead one's cache |
| `no_publisher_never_delivers` | none: the negative control | - | with nothing publishing, nothing is announced and no broadcast resolves |

A moq publisher is never blocked by a slow reader: `write_frame` is synchronous
and infallible, and a reader that falls behind is shed rather than waited for.
So "backpressure" in the first drill is measured as an unread backlog sitting
behind a live subscription, which is the state the cancel has to unwind.

That drill deliberately does not wait for the publisher to go idle. A relay
holds its upstream subscription for `TRACK_IDLE_LINGER` (30s in moq-net) after
its last local reader leaves, so a viewer who comes back does not pay for a
fresh upstream subscribe. Waiting that out would make this the slowest test in
the workspace in order to watch a deliberate delay elapse; the rejoin the drill
does assert is the half of that behavior worth grading.

The negative control is what keeps the rest honest. Three drills prove things by
reading a frame; if the harness could report success without data moving, they
would all pass for free.

### Killing the relay

The relay runs on its own tokio runtime and is killed by dropping it. Aborting
the `run` task is not enough, because `moq_relay::serve` spawns a task per
connection and those keep serving a relay whose accept loop is gone.

A dropped runtime sends no `CONNECTION_CLOSE`, exactly like a killed process, so
the clients discover the loss through the QUIC idle timeout. The drills set that
to two seconds (and the keep-alive well inside it), which is what keeps a crash
bounded rather than fast.

## Sensitivity

`sensitivity.sh` removes one recovery behavior at a time and requires the drill
covering it to fail. Each mutation is a patch under `mutations/`, applied to a
disposable copy of the tree; the checkout it runs from is never modified.

```bash
just test drill-sensitivity --list
just test drill-sensitivity reconnect-linger-disabled
```

| Mutation | Removes | Drill that must fail |
|---|---|---|
| `subscriber-leaks-broadcasts` | releasing the broadcasts a subscribing session fed when that session ends | `cancel_under_backpressure_releases_the_reader` |
| `reconnect-linger-disabled` | the linger window that carries a broadcast across a reconnect | `relay_killed_mid_group_aborts_then_resumes` |
| `relay-linger-never-expires` | the end of the relay's linger window for a vanished publisher | `interrupted_publisher_republishes_new_content` |

A mutated tree that fails to compile is a failure of the proof, not a pass: a
compile error shows the patch touched something, not that the drill was
watching. So is a drill that fails for a reason other than the one its mutation
names, which is why each patch declares the message its failure must carry.
The script runs every drill through nextest, whose process-level timeout also
terminates a mutation that wedges outside the drill's own Tokio timeouts.

Adding one: write the patch (a `git diff` of the behavior removed), give it the
two headers, and add a row above.

## What is underneath

A drill exercises a primitive end to end; the primitive's own orderings are
covered by the loom models, and the wire it rides by the fuzz targets. When a
drill fails, these are where the mechanism is pinned.

| Drill | Loom cases | Fuzz coverage |
|---|---|---|
| cancel under backpressure | `kio::loom::{last_consumer_wakes_unused, consumer_churn_resolves_unused, first_consumer_wakes_used}`, `moq-net tests/loom.rs::{subscriber_wakes_parked_demand, concurrent_tracks_drain_a_shared_pool}` | `lite` (`SUBSCRIBE` / `SUBSCRIBE_UPDATE` encodings) |
| relay killed mid-group | `kio::loom::{racing_last_producer_drops_still_close, queue_close_wakes_a_parked_pop, write_wakes_a_parked_consumer}`, `moq-net tests/loom.rs::publisher_drop_resolves_a_parked_subscriber` | `lite` and `ietf` (a truncated stream must fail to decode, never decode into something else), regression `ietf/subscribe-absolute-filter` |
| interrupted publisher republishes | `kio::loom::{write_wakes_a_parked_consumer, weak_upgrade_never_resurrects_a_closed_channel}`, `moq-net tests/loom.rs::back_to_back_groups_arrive_in_order` | `path` (the broadcast name is a path: normalization, prefixes, and the wire round-trip) |

Run them with `just rs loom` and `just rs fuzz <target>`. The committed fuzz
findings under `rs/moq-net/fuzz/regressions/` replay on stable as part of
`just test`, so a crash a drill leads you to belongs there rather than in a
corpus nobody replays.

## Not covered here

- An impaired path (delay, loss, rate limits). Loopback is the only path these
  drills see; `quest/m0/transport-impairment-profile.md` adds the Linux
  network-namespace profile they run under.
- CI lane scheduling, which belongs to `quest/m0/pr-behavioral-gates.md`.
- Failure bundles beyond what the test harness prints, which belongs to
  `quest/m0/qa-failure-artifacts.md`.
