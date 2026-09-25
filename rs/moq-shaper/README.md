# moq-shaper

A seeded userspace UDP impairment relay, so a test can put an impaired path in front of a server.

The shaper binds one socket, forwards every datagram to a target after a profile's treatment, and
forwards the replies back. A client dials the shaper instead of the server and reaches it through a
path with loss, a rate limit, delay, jitter, and reordering. QUIC is indifferent to the extra hop, so
this impairs a real transport with no capabilities and nothing touching the host's network, on macOS
and Linux alike. Kernel impairment (`netem` in a network namespace) is Linux only, needs
`CAP_NET_ADMIN`, and changes the host's network for everything running on it.

It is a datagram relay, not an HTTP interceptor or a TCP proxy, neither of which can impair QUIC.

## Usage

```bash
moq-relay --listen 127.0.0.1:4443 &

moq-shaper --listen 127.0.0.1:4444 --target 127.0.0.1:4443 \
    --delay 20ms --jitter 5ms --loss 0.05 --reorder 0.02 --rate 10000000 --seed 1
```

Point the client at the shaper's port. The flags build one profile, applied both ways. The seed and
profile print at start, and the counters at exit (Ctrl-C or SIGTERM), which exits nonzero if the
profile never acted.

| Flag | What it does |
| --- | --- |
| `--listen` | The address clients send to. |
| `--target` | The address every datagram is forwarded to. |
| `--seed` | Seeds every decision; the profile's own, or random, when omitted. |
| `--delay` | The base one-way delay. |
| `--jitter` | How far the delay varies: the most either way, or the sigma of a gaussian. |
| `--jitter-model` | `uniform` (the default) or `gaussian`. See [Jitter](#jitter). |
| `--loss` | The probability that a datagram is dropped. |
| `--reorder` | The probability that a datagram skips the delay, overtaking those in flight. |
| `--rate`, `--burst`, `--queue` | A token bucket: bits per second, the bytes it lets out ahead after an idle spell (1500), and the longest a datagram waits before it is dropped (100ms). |
| `--batch`, `--batch-window` | Hold datagrams until this many wait, or the window closes, then release them together. See [Batches](#batches). |
| `--shared` | Every client shares one link each way. See [Shared path](#shared-path). |
| `--profile` | A built-in profile's name, or a profile TOML file, in place of the flags that shape the path. See [Profiles](#profiles). |
| `--tcp-passthrough` | Also pipe TCP on the listening port to the target, untouched. See [TCP](#tcp). |
| `--report` | Write the profile, seed and counters to this file as JSON at exit. See [Report](#report). |
| `--report-interval` | Print the same JSON as one line on stdout this often. |

As a library, `Shaper::bind` takes a `Config`: where to listen and forward, the seed, and a `Profile`
per direction. It also takes a `Setup`, which is a `Config` plus the opt-in options below, each off by
default. `Shaper::verify` fails when the shaper stopped forwarding, or when an impairment the profile
configures never acted and the traffic makes that silence implausible. The relay's drills
(`rs/moq-relay/tests/drills.rs`, described in `test/drill/README.md`) run every scenario through it.

## What a seed does and does not fix

Every decision comes from the seed: each link draws loss, reorder and jitter from its own stream, in the
same order for every datagram whether or not the knob is on. Replay a run with the same seed and the
same datagrams are dropped, the same ones skip the delay, and each draws the same jitter.

The seed does not make the kernel's delivery clock reproducible: when a held datagram actually lands is
the host's scheduling. A test asserting on a replay should assert on the decisions, not on arrival times.

## Jitter

The default, `uniform`, draws each datagram's delay on its own within `jitter` either way of `delay`, so a
later datagram can overtake an earlier one. A drill wants that, and `jitter` may not exceed `delay`.

`gaussian` treats `jitter` as a sigma, clamps the draw at zero, and never lets a datagram leave before the
one in front of it: queueing delay on a FIFO path, which stretches and compresses the spacing and never
changes the order. Only `reorder` overtakes. This matters beyond realism: QUIC reads an overtake as loss,
retransmits and backs off, so a run meant to measure a jittery path ends up measuring the congestion
response. On a 5ms delay with 5ms of jitter, reordering alone dragged a receiver's audio buffer from
120ms to nearly two seconds.

## Shared path

Each client flow gets a link of its own each way by default: its own seeded stream, rate limit and queue,
so its decisions do not depend on anyone else's traffic.

With `shared`, every flow takes one link each way, the way clients behind one access link do. A browser
page with a WebTransport session of its own and another in its audio worker is two flows on one path:
shared, a gaussian path keeps the order across both, a batch fills from both, and one rate limit serves
both. A flow's treatment then depends on how its datagrams interleave with the others'.

## Batches

A paced hop, a Wi-Fi access point or a cellular scheduler bunches traffic: nothing moves for a while, then
several datagrams arrive at once. A batch holds datagrams until `count` are waiting, or until `window`
after the first arrived, then sends them all when the latest would have left. No delay or jitter produces
that clump, and it is what a receiver's jitter estimate has to cope with.

## Steps

A step changes a direction's delay, jitter, loss, reorder or rate limit once the run reaches `at`, counted
from when forwarding started. It changes only what it names, so a later step puts one knob back. A rate
step keeps the old bucket's credit, or its debt, clipped to the new one, and a limit that was not there
starts full. A step can add or change a rate limit, never remove one. Steps come from a profile file.

## Profiles

A profile file names both directions' treatment, the seed, and whether the path is shared. `up` is from
the client to the target, `down` the way back. Every field of a direction is optional and off when left
out, and unknown fields are refused.

```toml
name = "example"
seed = 7
shared = true

[up]
delay = "5ms"
jitter = "5ms"
jitter_model = "gaussian"  # required once the delay varies, through jitter or steps
loss = 0.02
reorder = 0.01             # skips the delay, so it needs one

[up.rate]
bits_per_second = 2000000
burst = 12500
queue = "250ms"

[up.batch]
count = 7
window = "160ms"

[[up.steps]]               # at 30s the delay grows,
at = "30s"
delay = "60ms"

[[up.steps]]               # and at 60s it recovers
at = "60s"
delay = "5ms"

[down]
delay = "5ms"
```

`--profile` takes a file's path, or one of the built-ins in `profiles/`, embedded in the binary. The
audio-quality lane (`test/audio-quality/`) runs the first five.

| Name | What it models |
| --- | --- |
| `near-zero` | The control. The shaper is in the path but treats nothing. |
| `mild` | A healthy wired LAN: 5ms delay, 5ms sigma, in order. |
| `bursty` | A paced hop: seven datagrams per 160ms window, released together. |
| `step` | 5ms for thirty seconds, then 60ms, with no recovery. |
| `high-rtt` | An intercontinental path: 75ms one way, 30ms sigma, in order. |
| `lossy` | 2% loss, with 1% of datagrams skipping a 20ms delay to overtake. |

Each built-in is a shared path, as a browser page's sessions on one host would be.

## TCP

A relay serves `/certificate.sha256` over HTTP on the port number it serves QUIC on, and a browser fetches
it before it dials WebTransport. With `tcp_passthrough`, the shaper listens for TCP on its own port and
pipes each connection to the target untouched, so the page loads through it. TCP is never impaired: a
reliable transport cannot shed load, so shaping it would only measure how TCP retransmits.

## Counters

`Shaper::stats` counts what each direction did, summed over every client.

| Counter | Meaning |
| --- | --- |
| `packets` | Datagrams received. |
| `lost` | Datagrams dropped by `loss`. |
| `overflowed` | Datagrams dropped because the rate limit's queue was full. |
| `throttled` | Datagrams that waited for the rate limit. |
| `delayed` | Datagrams given a nonzero delay, by the profile or by a batch holding them. |
| `reordered` | Datagrams sent ahead of the delay, overtaking any still in flight. |

## Report

`--report <file>` writes `{"profile", "seed", "up", "down"}` at exit, before the verdict, so a run that
fails it still leaves its counters behind. `profile` is the profile's name, or null when the flags built
it, and `up` and `down` hold the counters above. `--report-interval` prints the same object as one line on
stdout each period, which also survives a shaper killed before it could write the file.

A profile that silently did nothing turns an impaired run into an unimpaired pass. `verify` catches that
for the shaper's own exit; a harness grading a run should also check the report, where `near-zero` is the
only profile for which a `delayed` of zero is the right answer. Keep the name, the seed and the counters
with the run's artifacts: the seed is what turns a failing run into one that can be looked at again.
