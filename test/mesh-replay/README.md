# Mesh replay

Real `moq-relay` processes meshed over shaped links, played through the faults a
deployment has: a link reopening, a relay reopening every link it has, a crash, a
publisher leaving, and a roll that drains and replaces each relay's binary, one at a
time, between the newest release and this checkout. Publishers and readers run
through the whole script, and every event is reported by what it cost the mesh and
its readers.

The route simulator, its sweeps, and the route audit
(`rs/moq-net/tests/integration/route_audit/`) run the same failures on mock sessions
and mocked time; the drills (`test/drill/`) inject one fault into a relay or a pair.
This is the only place a mesh runs with releases mixed: mid-roll, two new relays
speak moq-lite-07 to each other and lite-06 to an old one, so one route crosses both.

```bash
just test mesh-replay                          # the nightly mesh and script, 4 broadcasts per relay
just test mesh-replay --broadcasts 4,32        # sweep broadcasts per relay
just test mesh-replay --broadcasts 256 --follow 2   # readers follow 2 of each relay's, so a big sweep costs routes, not readers
just test mesh-replay --mesh test/mesh-replay/ring4.toml --mesh test/mesh-replay/ring6.toml   # sweep relays
just test mesh-replay --mesh ~/our-mesh.toml   # a deployment's own graph
just test mesh-replay --new ./moq-relay-from-a-branch      # roll onto another build
```

## What runs

This checkout's relay, `moq-bench`, and `moq-shaper` build optimized, like the release
they roll against: debug relays on a 4-core runner fall behind at 32 broadcasts, and the
run then measures the build instead of the protocol.

Every relay listens on loopback with a QUIC idle timeout of 10s and a 10s drain, as
the defaults are, and publishes its stats so its `/metrics` counts routes and control
messages. A relay dials each link it owns through its own `moq-shaper`, which holds
datagrams behind a rate limit until its queue drops them: bufferbloat, never random
loss. The run's auth server grants every session the whole tree and marks a session
presenting the token `peer.<relay>` as a cluster peer.

On each relay, `moq-bench replay` publishes the relay's broadcasts
(`replay/<relay>/<n>`, one single-frame group every 100ms) and another follows every
other relay's, across every instance that serves it, the way a viewer waiting on a
stream would. The reader writes when its groups paused or skipped, and every second
its state, which is how a stall is told apart from a route that is gone.

## Files

The mesh file names the relays and the links; the dialing end owns each link:

```toml
name = "ring6"
relays = ["a", "b", "c", "d", "e", "f"]

[shape]          # every link, each way
delay = "10ms"
rate = 4000000   # bits per second
queue = "250ms"  # how long a datagram waits behind the rate limit before it is dropped

[[links]]
dial = "a"
accept = "b"
cost = 2         # optional, ?cost on the dialer's peer URL
upstream = true  # optional: `dial` marks `accept` upstream, on the new build only
shape = { delay = "40ms" }   # optional, over [shape]
build = "old"    # optional: only a dialer on this build dials it
```

A link naming a `build` models a roll that also changes the graph, such as a new
build that renders its dial set by a new rule: mid-roll, a link exists while its
dialer runs that build. The graph must be connected on either build, and at every
step of each scripted roll.

[`ring6.toml`](ring6.toml), the default and nightly mesh, is a ring of six with one
chord: its longest route crosses three links, every cut leaves a detour, and every
relay both dials and accepts. Nightly also runs [`ring4.toml`](ring4.toml), so the
report sweeps relays in the mesh as well as broadcasts per relay. A deployment
replays its own graph by passing its own file. Keep real topologies out of this
repository.

The script ([`script.toml`](script.toml)) is the sequence of events:

| Key | Meaning |
| --- | --- |
| `start` | The build every relay starts on: `old` or `new`. |
| `down` | How long a cut link or a crashed relay stays down. Longer than the idle timeout ends the session. |
| `quiet` | How long no route or announce message may move for the mesh to count as quiet. |
| `settle` | The longest an event waits for quiet and for every reader to read again. |
| `stall` | How long a reader may read nothing and still count as fed, once it read since the fault ended. |
| `[[events]]` | `roll` (`to = "new"` or `"old"`), `reopen-link` (`link = "a-b"`), `reopen-relay`, `kill`, `end-source` (each `relay = "c"`), with an optional `times`. An event naming no relay takes the one with the most links, and a `reopen-link` naming no link its first link every build dials. |
| `[[known]]` | A violation a quest already owns: `check`, its `kind`, optionally the event `kind`, and the `quest`. It is reported and does not fail the run, except at the start, before any fault. |

A roll drains each relay in the mesh file's order with SIGTERM and starts the other
build on its port once it exits. Reopening a link pauses its shaper for `down`, then
resumes it; reopening a relay pauses all of its shapers at once. A kill is SIGKILL,
then the same build after `down`. Ending a source stops one relay's publisher,
settles, then starts it again. After the script every publisher stops.

[`half-roll.toml`](half-roll.toml) rolls only `a` and `b` and stops: the middle of a roll,
where a new relay sits between a new one and an old one.
[`reopen.toml`](reopen.toml) runs the script's faults on the new build alone, with no roll.

## What fails a run

- **unfed**: once an event settles, a reader whose relay shows its live broadcast read
  nothing since the fault ended or for `stall`. `stuck` means its follow holds a
  broadcast whose track ended and never handed out another; `starved` means it holds
  one whose groups stopped. A reader still unfed from an earlier event, having read
  nothing since, is reported as unfed since that event and matched against its known
  entries, so one strand fails the run once, not again at every later event.
- **unrouted**: at the start, before any fault, a reader's relay does not show a live
  broadcast, so nothing behind it would be tested. A mesh starting on the new build with a link
  marked `upstream` may hide a route, so there it is only reported, as after every other
  event.
- **shown**: once an event settles, a relay still shows a broadcast whose publisher stopped.
- **hung**: a rolled relay still had not exited 20s after its 10s drain, so the run killed it.
- **died**: once an event settles, a relay, publisher, reader, or shaper the run meant to
  be running has exited.
- **noisy**: once every publisher stopped and the mesh settled, a route or announce
  counter still moves within twice the quiet window.

The counts are a report, never a gate on their size: a number moving from one night to
the next is something to look at, not a failure.

## The report

Each run writes `report.json` and `report.md` per cell (one mesh and broadcast count)
to its run directory or `--report`, and the nightly job adds them to its summary. A
passing run deletes its run directory, so keep its reports with `--report DIR`, or the
reports and logs together with `HARNESS_KEEP=1`. For each event:

- **Quiet**: seconds after the fault ended until the last route or announce message
  moved, once nothing more moved for `quiet`; **Fed**: until every reader with a route
  read a group sent after the fault ended.
- **Announce msgs / bytes**: control messages of the `announce` class, which carries
  ROUTE, ROUTE\_END, DEAD, and DRAIN on lite-07, sent by every relay.
- **Changed**, **Rewritten**, **DEAD**, **DRAIN**: the route counters
  (`moq_relay_route_*`, see `doc/concept/stats.md`), sent and received. They count only
  moq-lite-07 sessions, so relays with no link on it before or after the event are left
  out, and the column reads n/a when none had one.
- **Readers hit**: reader paths that paused longer than 0.5s, of them **bystanders**,
  whose relay and source were both outside the fault, the **longest** pause, and the
  groups **missed** within one instance.

For a roll it also reports, per link of the drained relay, whether the neighbour heard a
GOAWAY before the link closed (DRAIN) or only saw it close (DEAD), read from the
neighbour's log and, on the new build, its GOAWAY counter. A link the drained relay
dialed is starred: a release that predates
[moq-dev/moq#9158](https://github.com/moq-dev/moq/pull/9158) drains only the sessions it
accepted, so its starred links read DEAD.

Each link's version is the one its accepting relay logged negotiating on the newest
session there, not yet ended, that the dialer's running process opened (`down` when none,
or while the dialer is down). A build that does not log `negotiated version=` reads `down`
on every link it accepts. The report lists, for each
event that changed one, how many links are on each version and which moved.

`report.json` holds the same per relay, each link's version after each event, the worst
readers, and every violation.
