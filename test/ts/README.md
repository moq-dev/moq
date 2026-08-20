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
just test ts --with-eit         # add a synthetic EPG first, report which SI survived
```

`--analyze-only` needs only TSDuck + Python, so you can point it at any captured
subscriber output:

```bash
moq --connect http://localhost:4443 --broadcast live.hang export ts > sub.ts
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

## EIT fixtures

No capture in this repository carries EIT (PID 0x0012), so nothing exercises the
import path's handling of it. `make-eit-fixture.sh` synthesises an EPG onto any
transport stream, and `make-pending-eit.py` produces the one case a generator
cannot.

```bash
./make-eit-fixture.sh in.ts out.ts             # EIT p/f + schedule on PID 0x0012
./make-eit-fixture.sh --pf-only in.ts out.ts   # p/f only
./make-eit-fixture.sh --days 8 in.ts out.ts    # a guide at the DVB planning horizon
./make-pending-eit.py out.ts pending.ts        # ... whose tail is not yet in force
```

Everything is derived from the input, so the EIT describes the service the stream
actually carries: the triplet (`original_network_id`, `transport_stream_id`,
`service_id`) comes from its PAT and SDT, and the EPG is anchored to the stream's
own TDT where it has one. Without a TDT (the ffmpeg-generated clip has none) it
falls back to a fixed date, so the output is byte-reproducible for a given input
either way. The SDT's `EIT_present_following_flag` and `EIT_schedule_flag` are set
to match, since a stream carrying an EIT while advertising none is internally
inconsistent.

`tsp` replaces packets rather than creating them, so the EIT has to come out of
existing stuffing. A broadcast capture has plenty and keeps its exact mux rate; a
clip with none is padded to a constant bitrate first, and the script says so.

### Why `--days` matters

EIT schedule is sparse by construction, and that is the property most worth
testing against. A sub-table declares a `last_section_number` covering its whole
range and transmits only the segment-boundary sections that hold events, so
**completeness cannot be decided by counting sections**. `--days 8` reaches that
shape; the default twelve events does not. Censused with `--all-sections`:

| table\_id | distinct sections | declared `last_section_number` |
|---|---:|---:|
| 0x4E p/f | 2 | 1 |
| 0x50 schedule, days 0-3 | 32 | 248 |
| 0x51 schedule, days 4-7 | 32 | 248 |
| 0x52 schedule, days 8-11 | 3 | 16 |

An implementation that waits for section 248 to arrive before treating a schedule
sub-table as complete waits forever.

### Pending versions

`current_next_indicator` distinguishes the version in force from a revision that
applies later, and anything relaying SI must keep serving the current one until
its successor becomes current. `tsp -P eitinject` cannot generate that case: it
re-derives present/following from the event list and stamps its own version,
ignoring `version` and `current` in the input XML. `make-pending-eit.py` patches
the generated stream instead, bumping the version and clearing the indicator over
a trailing window with the section CRC recomputed, so a rejection downstream means
the guard fired rather than the section being malformed.

### Traps

Four ways to conclude the wrong thing here, each of which has cost time at least
once:

- **A table census hides sparse sub-tables.** `tsp -P tables` and `tstables` will
  not report a sub-table whose sections do not complete, which is every EIT
  schedule sub-table. Pass `--all-sections`, or schedule looks absent when it is
  present.
- **`--all-sections` cannot be combined with `--json-output` or `--xml-output`**
  (TSDuck rejects it), so a census built on structured output structurally cannot
  see those sub-tables. Parse the text form for that question.
- **Pending sections are excluded by default.** Add `--include-next`, or the
  fixture above looks like it did nothing.
- **A single TS packet never routes.** The import path's sync lock needs a
  successor before it will emit anything, so a Rust-level test that feeds one
  packet passes whatever the code does. Feed at least a pair, and include a
  positive control that would fail if the assertion were vacuous.

### Round-tripping a fixture

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

`.github/workflows/smoke.yml` runs `just test ts` and then `just test ts-eit`
after the interop matrix (nightly, on demand, and on PRs touching `test/ts/`).
The second recipe is `eit-roundtrip.sh`: it builds the sparse-schedule and
pending-version fixtures from a generated clip, round-trips them through a
relay, and censuses the capture, so a break in the generators or in the SI
carriage they pin fails a PR instead of landing silently. TSDuck comes from the
`nix develop` shell, so the run uses the same `tsp`/`tsanalyze` a local
developer would.

## Caveats

- Physical-layer TR 101 290 items (RF, real sync-byte loss) cannot be measured
  from a file; TSDuck notes the same limitation.
- Wall-clock delivery jitter/burstiness is intentionally out of scope: all timing
  is derived from the stream's PCR, not from socket arrival times.
- `tstd` models only the transport-buffer (TB) smoothing stage of the ISO 13818-1
  T-STD, not the full multiplex/elementary decode buffers. Its leak rates are
  defaults, not level-derived, so treat overflow as a smell rather than proof.
