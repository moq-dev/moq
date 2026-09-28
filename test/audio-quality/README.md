# Audio quality

Plays a tone in headless Chromium over a seeded, impaired UDP path and grades what the listener
would have heard: how often the ring ran dry, how much audio a re-anchor threw away, how much of the
run was silent, and where the playout delay settled. A regression in any of them fails a row
against [`budgets.json`](budgets.json) instead of arriving as a bug report.

```bash
just test audio-quality                                        # the whole matrix, 60s a row
just test audio-quality --list                                 # what it would run
just test audio-quality --profiles mild --codecs opus --duration 20
just test audio-quality --rings plain                          # the production path only
just test audio-quality --out ~/runs/after                     # keep the run directory
just test audio-quality --enforce                              # fail on the budgets
```

The matrix is codec x profile x ring: 16 rows, about 20 minutes. `--profiles`, `--rings`, and
`--codecs` take comma-separated lists; `--seed` replays a given impairment. The table prints either
way, and only `--enforce` (which the nightly job passes) turns a breach into a failed run.

## Rows

| Axis | Values |
| --- | --- |
| codec | `opus` at 48 kHz over fMP4, `aac` at 44.1 kHz over MPEG-TS |
| profile | see below |
| ring | `isolated` (SharedArrayBuffer, cross-origin isolated page) and `plain` (postMessage) |

`plain` is the production path: most pages are not cross-origin isolated. One build is served under
`/isolated/` with COOP and COEP and under `/plain/` without, and the page's `crossOriginIsolated`
decides which ring ran.

| Profile | Path | Delay |
| --- | --- | --- |
| `near-zero` | through the shaper, untreated | auto |
| `mild` | 5 ms one way, plus or minus 5 ms uniform | auto |
| `wide` | 40 ms one way, plus or minus 40 ms uniform | auto |
| `fixed-250` | through the shaper, untreated | a fixed 250 ms |

`fixed-250` is the control: every other row adapts, so without it the whole matrix could pass while a
fixed preset regressed, and a fixed preset is what many viewers land on. Loss, reorder, and rate
limits stay off: the buffer's job is absorbing arrival spread, and congestion response would make a
failure hard to attribute. The shaper's uniform jitter does reorder datagrams whose spacing is
smaller than it, so `wide` measures some reordering along with the spread. The bursty and mid-run
step profiles need shaper features that are not here yet
([quest](../../quest/m0/audio-quality-harness/shaper-profiles.md)).

The AAC arm keeps ffmpeg's default PES packing, whose multi-frame bursts are the flush-span shape
the reporter measured on the public relay (#3477). The source is a sine tone rather than a film, so a
quiet quantum at the output is always a gap or a stall and never the soundtrack.

## The path

```text
  page  ──UDP──▶  moq-shaper  ──▶  moq-relay     WebTransport, the impaired path
  moq (opus, aac)  ─────────────▶  moq-relay     the publishers, on the clean path
```

Every port is reserved for the run ([the harness contract](../README.md)), and the shaper is the
same `moq-shaper` binary the [transport drills](../drill/README.md) run under, so the two share one
impairment implementation. It exits nonzero when a configured impairment never acted, and that
voids the row.

The shaper carries UDP and nothing else. The page fetches `/certificate.sha256` from the address it
dials, so the driver answers that fetch with the relay's own hash (the certificate is pinned by
hash, so a different port needs nothing more). With nothing listening on TCP there, the WebSocket
fallback cannot connect: the session is WebTransport through the shaper, or the row is void.

## The metric contract

[`clients/js/src/schema.ts`](clients/js/src/schema.ts) is the contract. The page emits `Sample`s,
[`src/analyze.ts`](clients/js/src/analyze.ts) reduces a row to a `Summary`, and `grade.ts` reads that
against the budgets. A native lane or the latency ledger that emits the same `Summary` is comparable
by construction.

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

A row that cannot be trusted is void, reported rather than graded, and fails an enforced run.

| Void | Why |
| --- | --- |
| `shaper` | It exited nonzero (a configured impairment never acted), left no report, or forwarded nothing to the page. |
| `transport` | The session negotiated something other than WebTransport. |
| `ring` | `crossOriginIsolated` does not match the row's ring, so the other ring ran. |
| `clock` | The AudioContext clock was unreadable or ran more than 1% off the viewer's. |
| `window` | No audio after the warmup. |
| `driver` | The page never reached a session or first audio within 30 s, or the driver threw. |

## Budgets

`budgets.json` holds a ceiling per graded key per row, keyed by the whole row: runtime, codec, rate,
profile, and ring. Under `--enforce` a run fails on a breach, a budgeted key the run left
unmeasured, a void row, or a row with no budget. Tightening a ceiling is a visible diff; loosening
one needs a reason in review. The file's `note` says how the current ceilings were measured.

The nightly `Audio quality` job runs the matrix under `--enforce` and keeps a failing run directory
for a week: each process's log, the shaper's counters, every row's raw samples and summary.

## Layout

```text
run.sh                 builds, starts the relay and publishers, runs each row behind the shaper
relay.toml             anonymous relay, self-signed cert
budgets.json           a ceiling per metric per row
clients/js/
  index.html src/page.ts   the watch element, driven from the query string
  src/probe.ts             samples its public signals every 250 ms
  src/tap.ts               the listening worklet (tap-worklet.ts, quantum.ts)
  src/schema.ts            the metric contract
  src/analyze.ts           samples to a summary, unit tested
  driver.ts                one row in headless Chromium, and its void checks
  analyze.ts grade.ts      the summary per row, then the table and the verdict
```

## Attribution

The harness is the reporter's, from #3477: fperex built it on their fork `fperex/moq` (branches
`debug/rt-audio` and `debug-findings-solution`), with the black-box probe, the analyzer's skip-ahead
rule, the Playwright matrix over `moq-shaper`, the budget file graded under `--enforce`, and the
nightly job. This lane upstreams it on its own, without the fork's player, estimator, or shaper
changes: the underrun count comes from a tap on the output rather than a counter in the player, and
the profiles are the ones the shaper on `main` can express.

## Not covered

Safari and Firefox, video, and perceptual scoring: the grade is glitches and latency, not an opinion
about how it sounds. Recorded arrival traces are
[their own quest](../../quest/m0/audio-quality-harness/traces.md).
