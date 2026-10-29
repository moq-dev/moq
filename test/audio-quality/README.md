# Audio quality

Plays a tone in headless Chromium and in the native `moq play` over a seeded, impaired UDP path and
grades what the listener would have heard: how often the ring ran dry, how much audio a re-anchor
threw away, how much of the run was silent, and where the playout delay settled. A regression in
any of them fails a row against [`budgets.json`](budgets.json) instead of arriving as a bug report,
and a divergence between the two players shows up as one failing where the other passes.

```bash
just test audio-quality                                        # the whole matrix, 60s a row
just test audio-quality --list                                 # what it would run
just test audio-quality --profiles mild --codecs opus --duration 20
just test audio-quality --runtimes chromium                    # the browser player and the replays
just test audio-quality --runtimes native                      # the native player only
just test audio-quality --out ~/runs/after                     # keep the run directory
just test audio-quality --enforce                              # fail on the budgets
just test audio-quality --profiles bursty --capture --out ~/runs/burst # keep arrival evidence
just test audio-quality --replays                              # the replays only, in seconds
```

The matrix is runtime x codec x profile: 12 browser rows and 12 [native rows](#native-rows), about 30
minutes, plus 4 [replay rows](#traces) of recorded arrivals. `--profiles`, `--runtimes`, and `--codecs` take comma-separated
lists; `--seed` replays a given impairment. The table prints either way, and only `--enforce` (which
the nightly job passes) turns a breach into a failed run.

## Rows

| Axis | Values |
| --- | --- |
| runtime | `chromium` for `@moq/watch`, `native` for `moq play`; the recorded traces replay through the browser player |
| codec | `opus` at 48 kHz over fMP4, `aac` at 44.1 kHz over MPEG-TS |
| profile | see below |

| Profile | Path | Delay |
| --- | --- | --- |
| `near-zero` | through the shaper, untreated | auto |
| `mild` | 5 ms one way, 5 ms Gaussian sigma, in order | auto |
| `wide` | 40 ms one way, 40 ms Gaussian sigma, in order | auto |
| `bursty` | batches of seven datagrams, released within 160 ms | auto |
| `step` | 5 ms one way, stepping to 60 ms at 30 s | auto |
| `fixed-250` | through the shaper, untreated | a fixed 250 ms |

`fixed-250` is the control: every other row adapts, so without it the whole matrix could pass while a
fixed preset regressed, and a fixed preset is what many viewers land on. Loss, reorder, and rate
limits stay off: the buffer's job is absorbing arrival spread, and congestion response would make a
failure hard to attribute. The `mild` and `wide` profiles preserve datagram order and share one path across the page's sessions.
`bursty` and `step` use the shaper's named profiles on that same shared path. The step is timed from
shaper startup, before the page connects; the driver gives up on a row with no audio 20 s in, so the
measured window starts before the step, and `--duration` must exceed 30 s so playback spans it.

The AAC arm keeps ffmpeg's default PES packing, whose multi-frame bursts are the flush-span shape
the reporter measured on the public relay (#3477). The source is a sine tone rather than a film, so a
quiet quantum at the output is always a gap or a stall and never the soundtrack.

## The path

```text
  page or moq play  ──UDP──▶  moq-shaper  ──▶  moq-relay     WebTransport, the impaired path
  moq (opus, aac)  ─────────────────────────▶  moq-relay     the publishers, on the clean path
```

Every port is reserved for the run ([the harness contract](../README.md)), and the shaper is the
same `moq-shaper` binary the [transport drills](../drill/README.md) run under, so the two share one
impairment implementation. It exits nonzero when a configured impairment never acted, and that
voids the row.

The shaper carries UDP and nothing else. The page fetches `/certificate.sha256` from the address it
dials, so the driver answers that fetch with the relay's own hash (the certificate is pinned by
hash, so a different port needs nothing more). With nothing listening on TCP there, the WebSocket
fallback cannot connect: the session is WebTransport through the shaper, or the row is void.

## Native rows

The `native` runtime is the shipped binary, `moq play --video off`, through the same shaper on the same
profiles, writing [`--metrics`](../../doc/bin/cli.md#play) samples in the `Sample` shape below, which
the same analyzer reduces and the same budget file grades. It plays through the production
ALSA/cpal output into a private PulseAudio null sink ([`../lib/pulse.py`](../lib/pulse.py)), one per
row, as [`native-play`](../native-play/README.md) does, so a real device callback is part of what is
measured. The driver dials the shaper with the relay's certificate pinned by hash and voids the row
unless the player's log names WebTransport.

The counters come from the player's output callback rather than a tap on its output: it folds every
buffer it plays into 128-frame quanta at the device rate and classifies each by what the ring
supplied (`moq_audio::playback::Observer`). The meanings are the browser's, with three differences
in what the native player does:

- Its ring never stalls to refill. A dry ring underruns until the player pads it back to the target
  with silence, which `silence` counts, so native `stalled` is only a sink with nothing written yet.
- The playhead is projected from the player's last write at the wall rate and held at its end.
- `device` is the host's reported output latency, with no `baseLatency`, and `render_load` is
  unmeasured.

These rows grade end-to-end budgets with real device noise in them. They do not compare the two
players' target series, so a difference in totals alone says nothing about the estimator.

## Diagnosing live rows

Every browser sample includes the current PROBE RTT, Sync's network buffer, and the selected
rendition's advertised jitter and delay. These distinguish a target increase caused by the
transport from one declared by the publisher. The final page environment keeps the full audio
configuration.

`--capture` also writes `<tag>.arrivals.ndjson`: `[at, timestamp, group, duration]` on the same viewer clock as
`<tag>.ndjson`, before the container orders or skips groups. The observer shares the player's
connection and track, priority, and subscription max delay (at least 2 s at "auto", as the decoder
asks, not the target), so it adds no upstream subscription demand and still sees a frame later than
the target.
Groups drain concurrently, so an earlier stalled group cannot hide a later arrival. Capture
errors void the row. Capture adds local decoding and recording work; it is off by default and
on in nightly so a run preserves the inputs for diagnosis. It also records native main-thread
long tasks in `<tag>.longtasks.json`, on the same viewer clock. `--trace` implies capture and adds a
Chromium GC timeline in `<tag>.trace.json` and sampled CPU profile in `<tag>.profile.json`.
Tracing can perturb playback; compare it with a separate
untraced control and never use traced rows to calibrate quality budgets.

To replay a capture, put its arrivals and final environment configuration into the version 3
`Trace` below, subtract the first arrival's `at` from every arrival and from the last sample's
`at` for the duration, and supply the observed minimum RTT as a diagnostic. Replay measures the
target from the arrivals and holds the configuration constant; compare the raw samples when it
changed during the live run. The observer only
records frames delivered under the player's maximum age, so a replay cannot recover groups the
transport already discarded.

## Traces

The shaper's profiles are synthetic. [`traces/`](traces) holds real arrivals, each graded as a
`replay` row whose profile is the file's name:

| Trace | Recorded from |
| --- | --- |
| `relay-bbb` | the public relay's `bbb.hang`: one AAC frame per group, flushed in bursts of about seven, the flush-span shape #3477 measured |
| `local-aac` | this harness's AAC publisher through a local relay: the shallow control |
| `relay-mic` | `<moq-publish>` in headless Chromium, publishing its fake capture device through the public relay |
| `bursty-aac` | a nightly `bursty` AAC row's capture: each flush delivered newest first, which kept the player re-stalling until the estimator measured reordered frames |

A trace is a [`Trace`](clients/js/src/schema.ts): the catalog's audio rendition, the smallest
PROBE round trip the session saw, how long it observed, and every frame's `[at, timestamp, group, duration]`,
with `at` on the viewer's clock and `duration` in ms. Duration comes from the container's sample
metadata, the Opus packet TOC, or the codec's fixed sample count. Capture refuses a frame whose
duration cannot be determined; timestamp gaps never stand in for missing packet payloads.
`record.ts` stamps a frame as it comes off its group's stream, the way `Container.Consumer`
receives it, with the same transport, container, and codec-duration readers over WebTransport. It stamps before the consumer's in-order delivery, whose waits and skips depend
on the delay, so the replay decides those again.

`replay.ts` plays a trace through the player's own `Container.Consumer` and ring on a simulated
clock ([`js/watch/src/audio/replay.ts`](../../js/watch/src/audio/replay.ts)), at the "auto" delay a
real `Sync` resolves from the playout target that consumer measures, and reads each quantum through
the tap's classifier. The consumer makes the player's group ordering, max delay skips, and
discontinuity resets again at that delay, and the ring follows the delay as it moves; a group's stream is taken to finish with its last recorded frame. Every
frame plays for its recorded encoded duration, so missing packets stay missing audio even when
most frames never arrive. The target's codec term follows delivered packet durations, as the
decoder's does. Rendering runs to the end of the observation, so an outage after the last arrival is heard. It
writes the same samples a page does, so `analyze.ts` reduces both alike, and the shaper, transport,
and clock checks have nothing to void. Decoding is taken as instant. A replay is deterministic, so
its budgets are exactly what it measured.

```bash
just test audio-quality-record                        # the three recorded lanes, 35 s each, needs the network
just test audio-quality-record --lanes relay-bbb
```

Version 3 adds per-frame durations. The AAC-LC recordings made before that field existed retain
their arrival clocks and use the codec's fixed 1024 samples per frame. The Opus `relay-mic` recording
was replaced with a new fake-device capture because its old payloads, and therefore its packet
durations, were not retained. It still does not prove real-microphone behavior or added RTT.

Recording is by hand: the public relay and a browser publisher are not something a nightly run
should depend on. Re-measure the replay budgets after re-recording. The recorder asks the relay for
ten seconds of backlog so a late group is kept, which makes a trace's tune-in heavier than a
player's; the warmup excludes it from the grade. `bursty-aac` is not recorded here: it is a nightly
capture converted by the recipe [above](#diagnosing-live-rows).

## The metric contract

[`clients/js/src/schema.ts`](clients/js/src/schema.ts) is the contract. The page emits `Sample`s,
[`src/analyze.ts`](clients/js/src/analyze.ts) reduces a row to a `Summary`, and `grade.ts` reads that
against the budgets. `moq play --metrics` emits the same `Sample`s, so a native row is comparable by
construction, as is any other lane that emits the same `Summary`.

**Units.** Every duration is milliseconds as a float, every count an integer, every share a fraction
of 1. Samples missing at the device rate are converted to ms before they reach a summary.

**Clocks.** Every timestamp is taken on a named clock and reduced to `viewer`, the page's monotonic
`performance.now()`, before any two are subtracted:

| Clock | What | Reduced to `viewer` by |
| --- | --- | --- |
| `viewer` | `performance.now()` | nothing: it is the reference |
| `render` | the `AudioContext` frame clock | offset and rate fitted from paired readings every 250 ms |
| `media` | the publisher's timestamps, through the stream | rate fitted; the offset is unknowable from the viewer |
| `publisher`, `relay` | other processes | an offset measured NTP style over the session, uncertain by half a round trip |

Nothing in this lane carries a publisher or relay timestamp yet, so the stages that need one are
reported as unmeasured rather than guessed. A render clock more than 1% off the viewer's voids the
row: it is a throttled or stalled context, and everything counted on it would be wrong.

**Aggregation.** A budget key is `<metric>_<aggregation>`: `total` over the graded window,
`per_min` (the total over the window's minutes), `p50`/`p95` (nearest rank over the window's
samples), `max`, `share` (of the window), or `last`. So "underruns: 3" and "underruns: 3/min" are
different keys and are never graded against each other.

**Counters.** The player publishes no count of its own underruns, so the page fans a listening
AudioWorklet out from the player's output node. The player's render worklet copies what the ring
holds into the front of each 128-sample quantum and leaves the rest zero, so the tap reads every
quantum's fill level from the output itself:

- `underruns`: quanta the ring filled only partly, or not at all, while playing.
- `short_quanta`: the underruns that were partly filled: the ring ran dry part-way through.
- `underrun_episodes`: maximal runs of consecutive underruns, one audible gap each.
- `underrun_ms`: the missing audio, summed, and the longest episode as `max`.
- `skip_aheads`: re-anchors that discarded buffered audio, seen as a forward step of more than 40 ms
  in the lag between the viewer clock and the playhead (judged on the median of four samples either
  side, after removing the media clock's drift).
- `discarded_ms`: the media those skip-aheads jumped over. Audio dropped before it reached the ring
  moves no playhead, so it is not counted.
- `stalled`: the share of the window the ring spent stalled, refilling rather than playing. A gap
  that starts within 60 ms of a stall is that stall, and is not an underrun.
- `silence`: the share of rendered quanta under -60 dBFS at the output, whatever the cause.
- `target_ms`: the resolved playout delay (`sync.out.delay`).
- `converge_ms`: from first audio to the last time the target moved more than one 20 ms bucket from
  its final value.
- `render_load`: Chromium's render capacity, the share of each quantum's budget the graph used.

Every row discards the first 5 s after first audio, which is the tune-in rather than the steady
state; `converge_ms` grades the tune-in instead.

**Stages.** An end-to-end delay splits into exclusive spans, each defined by its two boundaries:
`capture`, `encode`, `publish_flush`, `network`, `jitter_buffer`, `decode`, `render`, `device`, and
the named remainder `unaccounted`. The identity is `sum == end-to-end` within the larger of 2 ms or
2%. This lane measures `jitter_buffer` (the resolved target) and `device` (`outputLatency` plus
`baseLatency`) on the viewer, and reports the publisher's declared `publish_flush` without summing it
with them. No clock spans the whole path here, so `end-to-end` is null rather than an identity that
holds by construction.

## Void rules

A row that cannot be trusted is void, reported rather than graded, and fails an enforced run. A
replay row crosses no shaper, so the `shaper` void does not apply to it.

| Void | Why |
| --- | --- |
| `shaper` | It exited nonzero (a configured impairment never acted), left no report, or forwarded nothing to the player. |
| `transport` | The session negotiated something other than WebTransport. |
| `clock` | The render clock (the AudioContext's, or the native device's) was unreadable or ran more than 1% off the viewer's from first audio on. |
| `window` | No audio after the warmup. |
| `driver` | The player never reached a session and first audio within 20 s, failed mid-row, did not exit cleanly on Ctrl-C (which includes `--metrics` falling behind its device), or the driver threw. |

## Budgets

`budgets.json` holds a ceiling per graded key per row, keyed by the whole row: runtime, codec, rate,
and profile. Under `--enforce` a run fails on a breach, a budgeted key the run left
unmeasured, a void row, or a row with no budget. Tightening a ceiling is a visible diff; loosening
one needs a reason in review. The file's `note` says how the current ceilings were measured: the
browser rows come from several nightly runs, not a developer machine; the native rows are provisional,
from a developer machine, until the nightly runner has measured them.

The nightly `Audio quality` job runs the browser rows under `--enforce` and the native rows reporting
only, until an intermittent two-second device stall that voids about one AAC native row in seven is
fixed. It keeps every run directory for a week as the `audio-quality-run` artifact: each process's log, the shaper's counters, every row's raw
samples and summary. The
replays also run under `--enforce` nightly on `main` and on every release PR that touches the
player's packages (`js/watch`, `js/hang`, `js/net`, `js/signals`) or the harness
(`.github/workflows/audio-quality.yml`). A change that moves them updates `budgets.json` in the
same PR, so run `just test audio-quality --replays --enforce` before pushing one.

## Layout

```text
run.sh                 builds, starts the relay and publishers, runs each row behind the shaper
../lib/pulse.py        the native rows' private PulseAudio null sink
record.sh              records traces/, by hand
relay.toml             anonymous relay, self-signed cert
budgets.json           a ceiling per metric per row
traces/                recorded arrivals, one replay profile each
clients/js/
  index.html src/page.ts   the watch element, driven from the query string
  src/probe.ts             samples its public signals every 250 ms
  src/tap.ts               the listening worklet (tap-worklet.ts, quantum.ts)
  src/schema.ts            the metric contract
  src/analyze.ts           samples to a summary, unit tested
  driver.ts                one row in headless Chromium, and its void checks
  native.ts                one row of `moq play --metrics`, and its void checks
  recorder.html mic.html   the recorder and the microphone publisher (src/recorder.ts, src/mic.ts)
  record.ts                one trace in headless Chromium
  replay.ts                the replay rows, written as a page's samples
  analyze.ts grade.ts      the summary per row, then the table and the verdict
```

## Attribution

The harness is the reporter's, from #3477: fperex built it on their fork `fperex/moq` (branches
`debug/rt-audio` and `debug-findings-solution`), with the black-box probe, the analyzer's skip-ahead
rule, the Playwright matrix over `moq-shaper`, the budget file graded under `--enforce`, and the
nightly job. The harness and opt-in shaper features preserve that author's credit. The player and estimator
changes remain separate: the underrun count comes from a tap on the output rather than a counter in the player, and
the profiles now include the fork's batch, step, and order-preserving jitter treatments.

## Not covered

Safari and Firefox, native playback on macOS and Windows, video, and perceptual scoring: the grade
is glitches and latency, not an opinion about how it sounds. The `relay-mic` trace captures Chromium's fake device rather than a real
microphone, so it carries a browser publisher's encode and send cadence but not a sound card's.
