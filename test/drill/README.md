# Transport failure drills

Media flowing on a healthy loopback proves delivery and nothing else. These
drills disrupt a live session through a real relay over real QUIC and check what
happens next: cancellation, a relay dying mid-group, a publisher coming back
under a name it already used, and bursts of small groups crossing a two-relay
cluster, with its peer link steady or flapping.

The drills themselves are ordinary Rust integration tests in
`rs/moq-relay/tests/drills.rs`, so `just check` already runs them whenever
moq-relay or anything under it changes. This directory holds the focused recipe
and the sensitivity proof.

```bash
just test drill                 # the drills, plus the negative control
just test drill relay_killed    # one of them, in both lanes
just test drill impaired        # every drill over the impaired path
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
| `bursts_cross_a_cluster` | Bursts of one-frame groups cross from an origin relay to a clustered edge relay; the subscriber reads newest-only and FETCHes every group it misses | how many groups missed the live subscription, which live groups were reset, and the slowest group's latency | every group arrives, live or by FETCH, within 10s of being written; a lost group fails by name (FETCH failed, FETCH unanswered, live group stalled), and the newest group comes live |
| `bursts_cross_a_flapping_peer` | The same bursts, with the peer link cut halfway through writing the second burst and restored once the edge withdraws the route | how long the link was down before the withdrawal, and the reads and FETCHes the flap failed | the route comes back and every group still arrives within 10s of being written; only a failure the flap caused is retried, on the restored route, so a hang on the dead one or a withdrawal with the link up still fails |
| `no_publisher_never_delivers` | none: the negative control | - | with nothing publishing, nothing is announced and no broadcast resolves |

A moq publisher is never blocked by a slow reader: `write_frame` is synchronous
and infallible, and a reader that falls behind is shed rather than waited for.
So "backpressure" in the first drill is measured as an unread backlog sitting
behind a live subscription, which is the state the cancel has to unwind.

That drill deliberately does not wait for the publisher to go idle. That
edge is the origin front's: it drops the source track when the last local
reader leaves, and is covered by moq-net's origin tests. Waiting out a
linger would make this the slowest test in the workspace; the rejoin the
drill does assert is the half of that behavior worth grading here.

The negative control is what keeps the rest honest. Five drills prove things by
reading a frame; if the harness could report success without data moving, they
would all pass for free.

### Killing the relay

The relay runs on its own tokio runtime and is killed by dropping it. Aborting
the `run` task is not enough, because the relay accept loop spawns a task per
connection and those keep serving a relay whose accept loop is gone.

A dropped runtime sends no `CONNECTION_CLOSE`, exactly like a killed process, so
the clients discover the loss through the QUIC idle timeout. The drills set that
to two seconds (and the keep-alive well inside it), which is what keeps a crash
bounded rather than fast.

## Two lanes

Every drill runs twice, as `<drill>::loopback` and `<drill>::impaired`. The
`lanes!` macro generates both from one body, so a scenario cannot drift between
them. In the impaired lane every client dials a `moq-shaper` in front of the
relay instead of the relay itself. The shaper is a userspace UDP relay that
forwards each datagram, both ways, after applying the same profile:

| Impairment | Value |
|---|---|
| delay | 20ms each way, plus or minus 5ms of uniform jitter |
| loss | 5% |
| reorder | 2% skip the delay and overtake whatever is in flight |
| rate | 10 Mbit/s token bucket, 1500 byte burst, 100ms queue then tail drop |

`bursts_cross_a_cluster` shapes all three hops, the publisher's, the peer link
between the relays, and the subscriber's, each with that profile behind a
100 kbit/s bottleneck with a 50ms queue, which a burst overruns. Its impaired
lane also grants less QUIC credit than a burst needs at every endpoint: 16
concurrent streams (each group is a stream, each FETCH another) and receive
windows of a few groups, so senders wait on `MAX_STREAMS` and `MAX_DATA` that
the path can lose. The bottleneck sits below the rate those windows allow, so
both bind, and the drill fails unless some burst overflowed a queue. Its
loopback lane keeps the defaults.

`bursts_cross_a_flapping_peer` cuts the peer link with `Shaper::cut`, which
drops every datagram both ways until it is restored, so its loopback lane puts a
shaper on that link too, one that forwards untouched. The cut lasts until the
subscriber sees the route withdrawn, which the relays' two-second idle timeout
bounds, rather than for a fixed time.

It is a datagram relay, not an HTTP or TCP proxy, neither of which can impair
QUIC. It needs no capabilities, works the same on macOS and Linux, and touches
nothing on the host; QUIC is indifferent to the extra hop.
The shaper runs on the test's runtime rather than the relay's, so killing the
relay leaves the path up, the way a network outlives the server behind it.

Every decision the shaper makes comes from one seed. Each run picks a fresh one
and prints it as `impaired: MOQ_SHAPER_SEED=...`; setting that variable replays
the same decisions. A drill with several paths seeds each one off that base.
Kernel scheduling still varies delivery timing, so the seed makes the decisions
reproducible, not the clock. Each client
draws from its own stream, numbered in the order clients first send, so
concurrent clients that race to connect can swap streams between runs.

The impairment is asserted, not assumed. The shaper counts what it lost,
throttled, delayed, and reordered, the drill prints those counters, and
`Shaper::verify` fails the run when an impairment the profile configures never
acted and the traffic makes that silence implausible (below one in 10,000). A
delay always acts, so one undelayed run fails outright, while a short run can
plausibly see no loss. A rate limit only bites on traffic that exceeds it, so
its counts are reported but never required. A profile that silently did nothing
would turn this lane into a second loopback run that passes for free.

There are no retries: a drill that only passes on loopback is a finding.

The same shaper is a binary, for putting in front of a relay from another
process. It applies one profile both ways, prints the seed at start and the
counters at exit (Ctrl-C or SIGTERM), and exits nonzero if the profile never
acted:

```bash
cargo run -p moq-shaper -- --listen 127.0.0.1:4444 --target 127.0.0.1:4443 \
    --delay 20ms --jitter 5ms --loss 0.05 --reorder 0.02 --rate 10000000 --seed 1
```

Kernel-real impairment (`netem` in a network namespace) is deliberately not
used: it is Linux only, needs `CAP_NET_ADMIN`, and the drills grade the
protocol's reaction to loss and delay, not the kernel's rendering of them.

## Sensitivity

The Nightly workflow runs all mutations, so patches that stop applying and drills
that stop detecting their recovery failures fail CI. `just check` also runs
`--apply-only` whenever Rust or this directory changes, so a patch that no longer
applies fails the PR that moved its code rather than the next nightly.

`sensitivity.sh` removes one recovery behavior at a time and requires the drill
covering it to fail. Each mutation is a patch under `mutations/`, applied to a
disposable copy of the tree; the checkout it runs from is never modified.

```bash
just test drill-sensitivity --list
just test drill-sensitivity reconnect-stops-after-session-loss
```

Each mutation names its drill's `loopback` lane, which is the faster of the two
and grades the same body.

| Mutation | Removes | Drill that must fail |
|---|---|---|
| `subscriber-leaks-broadcasts` | releasing the broadcasts a subscribing session fed when that session ends | `cancel_under_backpressure_releases_the_reader::loopback` |
| `reconnect-stops-after-session-loss` | redialing after an established session is lost | `relay_killed_mid_group_aborts_then_resumes::loopback` |
| `relay-withdraws-lost-publisher` | withdrawing a publisher's announcements when its session is lost | `interrupted_publisher_republishes_new_content::loopback` |
| `fetch-never-sent` | sending a FETCH for an uncached group, at the subscriber and at the edge relay | `bursts_cross_a_cluster::loopback` |
| `peer-link-never-redialed` | redialing a cluster peer after its link is lost | `bursts_cross_a_flapping_peer::loopback` |

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
| bursts across a cluster, steady or flapping | `moq-net tests/loom.rs::{a_fetch_never_joins_a_withdrawn_attempt, back_to_back_groups_arrive_in_order}` | `lite` (`FETCH` encodings) |

Run them with `just rs loom` and `just rs fuzz <target>`. The committed fuzz
findings under `rs/moq-net/fuzz/regressions/` replay on stable as part of
`just check`, so a crash a drill leads you to belongs there rather than in a
corpus nobody replays.

## Not covered here

- CI lane scheduling.
