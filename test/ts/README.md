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

## Running

```bash
just test ts                    # generate a clip, round-trip, analyze
just test ts --source cap.ts    # round-trip a real capture instead
just test ts --analyze-only x.ts # skip the round-trip, analyze a file
just test ts --strict           # also fail on broadcast-shape warnings
```

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

## CI

`.github/workflows/smoke.yml` runs `just test ts` after the interop
matrix (nightly, on demand, and on PRs touching `test/ts/`). TSDuck
comes from the `nix develop` shell, so the run uses the same `tsp`/`tsanalyze` a
local developer would.

## Caveats

- Physical-layer TR 101 290 items (RF, real sync-byte loss) cannot be measured
  from a file; TSDuck notes the same limitation.
- Wall-clock delivery jitter/burstiness is intentionally out of scope: all timing
  is derived from the stream's PCR, not from socket arrival times.
- `tstd` models only the transport-buffer (TB) smoothing stage of the ISO 13818-1
  T-STD, not the full multiplex/elementary decode buffers. Its leak rates are
  defaults, not level-derived, so treat overflow as a smell rather than proof.
