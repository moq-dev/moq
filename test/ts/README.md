# TS / IRD compliance test

Validates the MPEG-TS that the moq subscriber emits (`moq ... export ts`) against
what an Integrated Receiver/Decoder (IRD) expects. It round-trips a stream
through a relay (`import ts` -> relay -> `export ts`), captures the output, and
runs [TSDuck](https://tsduck.io) plus a custom analyzer over it.

This is a diagnostic gate, not just a pass/fail: the exporter
([`rs/moq-mux/src/container/ts/export.rs`](../../rs/moq-mux/src/container/ts/export.rs))
is VBR, inserts no null packets, and paces PCR once per media frame, so several
broadcast-shape checks are expected to flag. The report quantifies exactly where
and by how much.

Two instruments live here. `compliance.py` (via `run.sh`) grades a captured file
against the IRD model. [`pcr-timing.py`](#pcr-timing-pcr-timingpy) grades a live
pipe, which is the only way to see *when* the exporter released each PCR.

## Running

```bash
just test ts                    # generate a clip, round-trip, analyze
just test ts --source cap.ts    # round-trip a real capture instead
just test ts --analyze-only x.ts # skip the round-trip, analyze a file
just test ts --strict           # also fail on broadcast-shape warnings
just test ts --with-eit         # add a synthetic EPG first, report which SI survived
just test ts --live             # grade PCR release timing off the live pipe
```

`--live` swaps the analyzer, not the rig: the same round-trip runs, but the
subscriber's stdout goes straight into `pcr-timing.py` instead of a capture file.
That is the only arm that can see release timing at all, and it is what nightly
runs (see [CI](#ci)).

`--analyze-only` needs only TSDuck + Python, so you can point it at any captured
subscriber output:

```bash
moq --client-connect http://localhost:4443 --broadcast live.hang export ts > sub.ts
./run.sh --analyze-only sub.ts
```

Requirements: `tsp` and `tsanalyze` (TSDuck) and `python3` for every mode; the
round-trip modes also need `cargo`, `ffmpeg`, `curl`, and `timeout`.

## Checks

TSDuck parses the stream (`tsanalyze --json` for structure/PSI/services, and a
188/204-byte header scan for the per-packet PID + PCR timeline); the analyzer
does the model math. Timing is on the stream's own PCR clock (an IRD locks to
PCR), so no wall-clock capture is needed and results are deterministic per file.

Severities: **hard** checks fail the run by default; **shape** checks report as
`WARN` and only fail under `--strict`.

| Check | Severity | What it verifies |
|---|---|---|
| `packet-size` | hard | 188 (or 204) bytes per packet |
| `sync` | hard | no invalid sync bytes / transport-error packets |
| `pat` / `pmt` | hard | valid PAT mapping programs to a PMT that lists the elementary streams |
| `psi-crc` | hard | no section dropped for a bad CRC |
| `continuity` | hard | no continuity-counter discontinuities |
| `pcr-presence` | hard | a PCR PID is declared and carries PCR |
| `pcr-monotonic` | hard | PCR strictly increases (one 33-bit wrap tolerated) |
| `duration-fidelity` | hard | exported PCR span tracks the source's duration (round-trip only) |
| `pcr-repetition` | shape | consecutive PCRs within the limit (default 40 ms) |
| `pcr-jitter` | shape | per-interval PCR jitter vs the nominal bitrate (pcrverify model) |
| `null-ratio` | shape | null/stuffing fraction (flags only a pathological excess) |
| `service-descriptors` | shape | an SDT naming the service is present |
| `bitrate-consistency` | shape | instantaneous-bitrate spread over 1 ms / 10 ms windows (CBR-ness) |
| `burstiness` | shape | peak/mean of windowed delivery |
| `inter-arrival` | shape | packet inter-arrival spread on the PCR clock (informational) |
| `tstd` | shape | transport-buffer smoothing (TB fills on arrival, leaks at Rx) |

Every timing check reads the stream's own PCR, so a PCR emitted on the wrong
clock rate stays internally consistent and passes them all. `duration-fidelity`
is the exception: it compares the exported PCR span against the source's
independent duration, which pins the absolute rate. It runs only on a round-trip
(where a source exists); `run.sh` passes the source automatically, and
`--analyze-only` skips it.

Thresholds are CLI flags forwarded through `run.sh` (e.g.
`--pcr-repetition-ms`, `--pcr-jitter-us`, `--bitrate-cov-max`, `--burstiness-max`,
`--tb-size-bytes`, `--video-leak-bps`, `--audio-leak-bps`). `--report-json <path>`
writes the full machine-readable report.

## PCR timing (`pcr-timing.py`)

A PCR makes three claims at once, and an instrument pointed at one of them
cannot see the other two:

| Domain | What it claims | Graded from |
|---|---|---|
| value | consecutive PCR values are spaced within the repetition limit | the values |
| release | the bytes carrying a PCR were handed over when that PCR asserts | arrival stamps |
| position | a PCR packet sits among the media bytes it describes | packet offsets |

`compliance.py` grades `value` from a file, deterministically and with no
wall-clock capture, which is the right basis for the model math it does. That
also means it cannot grade `release`: a change to *when* the exporter hands bytes
over is invisible to any harness that does not stamp arrivals.
`pcr-timing.py` reads a pipe and grades all three in one pass.

```bash
# live: all three domains, reading the exporter directly
moq --client-connect http://localhost:4443 --broadcast live.hang export ts \
  | ./pcr-timing.py --live --seconds 45

# offline: value and position only (a file has no release timing left in it)
./pcr-timing.py capture.ts
```

It needs only `python3` — no TSDuck, no source file and no declared mux rate,
because every check is graded against the stream's **own** PCR values. If two
consecutive PCRs are 25 ms apart in value then they must be ~25 ms apart in
arrival, whatever clock rate the stream is running at. The price of that basis is
the same one `compliance.py` pays: a PCR emitted at the wrong rate stays
internally consistent, so absolute rate is not what this grades.

| Check | Severity | What it verifies |
|---|---|---|
| `sync` | hard | no invalid sync bytes / transport-error packets |
| `continuity` | hard | no discontinuities, and a payload-less packet must not advance the counter (ISO 13818-1 2.4.3.3) |
| `pcr-single-pid` | hard | every PCR rides one PID |
| `pcr-value-interval` | hard | no interval above `--repetition-ms` (default 40, TR 101 290) |
| `pcr-release-timing` | hard | each interval's arrival is within `--release-ms` of the interval its own values assert, with bounded accumulated drift (`--live` only) |
| `pcr-position` | shape | share of PCR packets within `--adjacent-packets` of the previous one |

`pcr-position` is a shape check because `export ts` is VBR by design. It is worth
reporting even so: a consumer holding only the byte stream — which is every
MPEG-TS tool — recovers the clock from where the PCR packets sit, so a layout
that clusters them and heaps the media bytes between the clusters is one such a
consumer cannot follow, however exact the values are.

When both `release` and `position` flag, the report cross-tabulates them:

```text
  release timing by byte position (report only)
    adjacent + early       615  ( 34.0%)
    adjacent + on time     408  ( 22.5%)
    spaced   + on time     641  ( 35.4%)
    spaced   + late        136  (  7.5%)
```

Two invariants failing on the *same* PCRs is one cause rather than two, which two
aggregate percentages cannot show. It is report-only: it explains a failure, it
does not define one.

`--report-json <path>` writes the full report, and `--strict` promotes the shape
check to hard.

## EIT fixture

`make-eit-fixture.sh` adds a synthetic DVB EPG to any transport stream, so the
import path's EIT handling can be exercised without a broadcast capture that
carries one. No capture in this repository does.

```bash
./make-eit-fixture.sh in.ts out.ts             # EIT p/f + schedule on PID 0x0012
./make-eit-fixture.sh --pf-only in.ts out.ts   # p/f only
```

Everything is derived from the input, so the EIT describes the service the
stream actually carries: the triplet (`original_network_id`,
`transport_stream_id`, `service_id`) comes from its PAT and SDT, and the EPG is
anchored to the stream's own TDT where it has one. Without a TDT (the
ffmpeg-generated clip has none), it falls back to a fixed date, so the output is
byte-reproducible for a given input. The SDT's `EIT_present_following_flag` and
`EIT_schedule_flag` are set to match, since a stream carrying an EIT while
advertising none is internally inconsistent.

`tsp` replaces packets rather than creating them, so the EIT has to come out of
existing stuffing. A broadcast capture has plenty and keeps its exact mux rate; a
clip with none is padded to a constant bitrate first, and the script says so.

`--with-eit` wires this into the round-trip and prints which SI PIDs came back:

```text
### SI round-trip (source -> capture)
  TABLE      PID           SOURCE      CAPTURE
  NIT        0x0010             7            5
  SDT        0x0011            31           21
  EIT        0x0012         1,007            0
  TDT/TOT    0x0014            40            0
```

This is a report, not a gate. `SI_PIDS` in
[`catalog.rs`](../../rs/moq-mux/src/container/ts/catalog.rs) is the allowlist of
PIDs the import path routes, and a table outside it is dropped by design rather
than by malfunction; the census makes that visible instead of leaving it to be
inferred from the code. Counts differ for a table that *did* survive because the
exporter re-emits SI on its own repetition cadence rather than the source's.

## CI

`.github/workflows/smoke.yml` runs `just test ts` after the interop
matrix (nightly, on demand, and on PRs touching `test/ts/`). TSDuck
comes from the `nix develop` shell, so the run uses the same `tsp`/`tsanalyze` a
local developer would.

`.github/workflows/nightly.yml` runs `just test ts --live` as well. It stays off
the PR path because release timing needs a real-time window to measure: the
grader has to sit on the pipe for the length of the capture, which no per-PR gate
should be paying for, and a scheduled arm on a quiet runner is a better place to
notice the exporter drifting off its own clock anyway.

## Caveats

- Physical-layer TR 101 290 items (RF, real sync-byte loss) cannot be measured
  from a file; TSDuck notes the same limitation.
- Wall-clock delivery jitter/burstiness is out of scope *for `compliance.py`*:
  all of its timing is derived from the stream's PCR, not from arrival times.
  `pcr-timing.py --live` covers that axis separately, by stamping a pipe.
- `tstd` models only the transport-buffer (TB) smoothing stage of the ISO 13818-1
  T-STD, not the full multiplex/elementary decode buffers. Its leak rates are
  defaults, not level-derived, so treat overflow as a smell rather than proof.
