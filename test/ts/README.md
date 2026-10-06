# TS / IRD compliance test

Validates the MPEG-TS that the moq subscriber emits (`moq ... export ts`) against
what an Integrated Receiver/Decoder (IRD) expects. It round-trips a stream
through a relay (`import ts` -> relay -> `export ts`), captures the output, and
runs [TSDuck](https://tsduck.io) plus a custom analyzer over it.

This is a diagnostic gate, not just a pass/fail: the exporter
([`rs/moq-mux/src/container/ts/export.rs`](../../rs/moq-mux/src/container/ts/export.rs))
pads with null packets to the multiplex rate the source recorded (or `--mux-rate`),
leaves a source without one unpadded, never delays media to fit the rate, and
puts a PCR on its own packet every 25 ms of media time, so several
broadcast-shape checks are expected to flag. The report quantifies exactly where
and by how much.

Four instruments live here. `compliance.py` (via `run.sh`) grades a captured file
against the IRD model. [`pcr-timing.py`](#pcr-timing-pcr-timingpy) grades a live
pipe, which is the only way to see *when* the exporter released each PCR.
[`table-anchor.py`](#table-anchoring-table-anchorpy) grades two exporters of one
broadcast against each other, which is the only way to see whether a table's
emission points come from the media or from the exporter's own clock.
[`open-gop.py`](#open-gop-open-goppy) grades an open-GOP capture against its
source, access unit by access unit.

## Running

```bash
just test ts                    # generate a clip, round-trip, analyze
just test ts --source cap.ts    # round-trip a real capture instead
just test ts --analyze-only x.ts # skip the round-trip, analyze a file
just test ts --strict           # also fail on broadcast-shape warnings
just test ts --with-eit         # add a synthetic EPG first, report which SI survived
just test ts --live             # grade PCR release timing off the live pipe
just test ts --pair             # two exporters of one broadcast, grade table anchoring
just test ts --open-gop         # open-GOP clip; its leading pictures must survive
```

`--live` swaps the analyzer, not the rig: the same round-trip runs, but the
subscriber's stdout goes straight into `pcr-timing.py` instead of a capture file.
That is the only arm that can see release timing at all, and it is what nightly
runs (see [CI](#ci)).

The default arm runs `pcr-timing.py` over its capture too, after `compliance.py`,
for the PCR checks `compliance.py` leaves to it: the value interval (hard), and
whether the bytes between consecutive PCRs are the ones the mux rate implies
([`pcr-schedule`](#byte-schedule), a shape check that reports without gating
unless `--strict`).

The live arm passes only when the grader's verdict *and* the publisher's exit status
are clean. The grader can only speak for what reached it, and the sample floor
rejects a window that came up short, so a publisher dying late in the run leaves
enough behind to pass every check: a broken round-trip reported as a good one.

`--analyze-only` needs only TSDuck + Python, so you can point it at any captured
subscriber output:

```bash
moq --connect http://localhost:4443 --broadcast live.hang export ts > sub.ts
./run.sh --analyze-only sub.ts
```

Requirements: `tsp`, `tsanalyze` and `tstables` (TSDuck) and `python3` for every mode; the
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
| `pcr-presence` | hard | every program's declared PCR PID carries PCR (0x1FFF declares none) |
| `pcr-monotonic` | hard | PCR strictly increases (one 33-bit wrap tolerated), except into a PCR that signals `discontinuity_indicator` |
| `duration-fidelity` | hard | exported PCR span tracks the source's duration (round-trip only) |
| `service-descriptors` | shape | an SDT naming the service is present |
| `tstd` | shape | the full T-STD buffer model: no TB, MB, EB or B overflow, no access unit incomplete at its decoding time (see [T-STD](#t-std)) |

Every timing check reads the stream's own PCR, so a PCR emitted on the wrong
clock rate stays internally consistent and passes them all. `duration-fidelity`
is the exception: it compares the exported PCR span against the source's
independent duration, which pins the absolute rate. It runs only on a round-trip
(where a source exists); `run.sh` passes the source automatically, and
`--analyze-only` skips it.

`compliance.py` grades what TSDuck parses and the T-STD model, and leaves PCR
spacing, byte schedule and release timing to `pcr-timing.py`, which honours
signalled discontinuities. `--report-json <path>` writes the full
machine-readable report.

## T-STD

`tstd` runs every audio and video stream through the ISO 13818-1 system target
decoder (2.4.2; Rec. ITU-T H.222.0, whose 10/2014 edition is a free download),
fed on the stream's own PCR clock:

```text
video (AVC 2.14.3.1, HEVC 2.17.2)   TB --Rx--> MB --Rbx (leak)--> EB --DTS--> decoder
audio (2.4.2.3)                     TB --Rx--> B  ----------------PTS--> decoder
```

It fails a stream where TB, MB, EB or B overflows, where TB stays occupied for a
second, where an access unit is not wholly in EB/B at its decoding time
(underflow), or where a byte waits longer than the STD delay bound (1 s, 10 s for
AVC/HEVC). The report gives each stream's peak fill per buffer, how late the worst
underflowed access unit finished arriving, and the longest any access unit waited.

No maintained tool implements this. TSDuck has no T-STD analyzer, and nothing else
in nixpkgs does either, so the model is hand-rolled and its parameters are
transcribed from the specs: H.264 Table A-1 and H.265 Table A.8 for the level, the
ADTS and "other audio" rates and sizes in H.222.0 2.4.2.3, and ATSC A/52 and A/53
Part 5 for AC-3 and E-AC-3. TSDuck does the parsing: `tstables` decodes the PMT
and `tsp -P pes --avc-access-unit` the SPS, AVC and HEVC alike.

Video takes its buffers from the HRD the SPS declares, as H.222.0 2.14.3.1
(AVC) and 2.17.2 (HEVC) specify. A NAL HRD sets Rx from its bit rate and EB to
its CPB size, and MB grows by whatever the level's CPB leaves over; Rbx stays the
level's. Without one, the level's limits stand in. A VCL HRD describes the VCL
alone, not the byte stream EB holds, so a stream declaring only that takes the
level defaults too. An `AVC_timing_and_HRD_descriptor` or
`HEVC_timing_and_HRD_descriptor` with `hrd_management_valid` switches MB-to-EB
transfer to the HRD's own schedule, which is not modelled, so such a stream is
refused.

Opus is graded against ADTS's buffers for the same channel count: the Opus-in-TS
draft gives Rx (2 Mb/s for 1-2 channels, matching ADTS) but leaves the buffer size
unset, so that size is borrowed rather than specified. A stream with no parameters
at all is refused by name rather than skipped, which fails the check: MPEG-1/2
video, DVB E-AC-3, and HEVC beyond Main/Main 10. Sections and private data
(SCTE-35, teletext) have no elementary-stream buffers and are listed as not
graded. A signalled PCR discontinuity starts fresh buffers, since the timestamps
on either side of it are on different clocks.

Every packet on an elementary stream's PID enters TB, including adaptation-only
ones (a PCR, stuffing) and legal duplicates (2.4.3.3: the same counter and
payload twice); only PES bytes of a first copy go on to MB/B. Video access units
are read from the ES rather than taken from PES boundaries: one opens at the first
delimiter, parameter set or SEI after the previous picture's slices (H.264
7.4.1.2.3, H.265 7.4.2.4.4), and H.222.0 requires a delimiter in each. A PES may
carry several, but only the first takes its timestamp, and deriving the rest from
the stream's own timing is not modelled, so that layout is refused. Video decode
times must strictly increase.

Audio bytes enter B as they leave TB, so a frame that ends partway through a packet
is complete once its own last byte has left, not the packet's. Video keeps one
simplification, toward strictness: a packet's bytes reach MB when its last byte
leaves TB, at most one packet's drain time later than byte by byte.

Once a stream's last packet is in, every access unit it completed is still graded
through its decoding time, however long after the capture that falls, so a burst
that arrives far ahead is held over the delay bound rather than missed. The last
access unit is usually cut off by the end of the capture; it is counted as
`truncated_units` and not graded, since its missing bytes were never sent rather
than late.

### Controls

`just test ts-tstd` (`tstd-controls.py`) proves the model can tell a compliant
stream from a broken one. The positive control is a real broadcast encoder's
output (`kyrion_dirtystart.ts` from the `moq-mux` test data: AVC High@4.0 with a
1.935 Mb/s CBR NAL HRD and a 755 kbit CPB, plus two MPEG-1 Layer II tracks), which
passes against its own declared buffer, filling EB to the brim as a CBR stream
should. The negatives restamp its PCRs with
`tsp -P pcradjust`, leaving every PES and timestamp alone, so only delivery
changes:

| Case | Expected |
|---|---|
| as captured | pass |
| PCRs restamped at the capture's own rate | pass |
| delivered at 0.7x | EB and B underflow |
| delivered at 4x | TB and B overflow, audio held over 1 s |
| delivered at 15x (a burst) | TB and B overflow, audio held over 1 s |

No restamp can overflow the video MB: it holds the level's whole CPB less the
declared one, about 3.6 MB, more than the 4 s capture carries.

The packet layouts a capture cannot be edited into are built synthetically: a
10 Mb/s single-video stream carrying the Kyrion SPS, one access unit per PES, with
a PCR packet between them. Each case fails without the handling it names:

| Case | Expected |
|---|---|
| as built | pass |
| two access units in one PES | refused |
| four adaptation-only packets after an access unit | TB overflow |
| an access unit's first packet sent twice | pass |
| the PMT declares a PCR PID that carries no PCR | `pcr-presence` fails |

A synthetic MPEG audio stream packs four 576-byte frames per PES, so frames end
partway through packets, and decodes each PES's first frame 0.3 ms after its last
byte leaves TB, before the rest of that packet has. It must pass, its last frame
cut off by the end of the capture.

The ffmpeg clip `run.sh` generates is not a positive control: its muxer sends
audio 0.7 s ahead by default (`-muxdelay`), which overflows the 3,584-byte ADTS
buffer, and even at 0.1 s a four-packet audio burst overflows TB.

### Gate

`tstd` is a shape check, so `just test ts` reports it without failing. `export
ts` output does not pass yet: some video access units arrive after their DTS, and
audio runs far enough ahead, in bursts, to overflow both TB and B. The change that
makes the exporter release on a fixed delay promotes `tstd` to a hard check.

## PCR timing (`pcr-timing.py`)

A PCR makes three claims at once, and an instrument pointed at one of them
cannot see the other two:

| Domain | What it claims | Graded from |
|---|---|---|
| value | consecutive PCR values are spaced within the repetition limit | the values |
| release | the bytes carrying a PCR were handed over when that PCR asserts | arrival stamps |
| position | a PCR packet sits among the media bytes it describes | packet offsets |

`pcr-timing.py` grades `value` and `position` from a file, and all three from a
pipe. A file carries no arrival stamps, so a change to *when* the exporter hands
bytes over is invisible to any harness that does not stamp them.

A constant-rate stream makes a fourth claim, graded by `pcr-schedule`: that the
bytes between consecutive PCRs are the bytes the mux rate implies for that
interval.

```bash
# live: every domain, reading the exporter directly
moq --connect http://localhost:4443 --broadcast live.hang export ts \
  | ./pcr-timing.py --live --seconds 45

# offline: value, position and schedule (a file has no release timing left in it)
./pcr-timing.py capture.ts --mux-rate 10000000
```

It needs only `python3`, with no TSDuck, no source file and no declared mux rate,
because every check is graded against the stream's **own** PCR values. If two
consecutive PCRs are 25 ms apart in value then they must be ~25 ms apart in
arrival, whatever clock rate the stream is running at. The price of that basis is
the same one `compliance.py` pays: a PCR emitted at the wrong rate stays
internally consistent, so absolute rate is not what this grades. `pcr-schedule`
is the exception when it is given `--mux-rate`, which pins the rate the way
`duration-fidelity` does; without it, it estimates the rate from the capture and
grades only how evenly the bytes are laid over the PCRs.

| Check | Severity | What it verifies |
|---|---|---|
| `sync` | hard | no invalid sync bytes / transport-error packets (`--live` only) |
| `continuity` | hard | no discontinuities, and a payload-less packet must not advance the counter (ISO 13818-1 2.4.3.3) (`--live` only) |
| `pcr-value-interval` | hard | no interval above `--repetition-ms` (default 100, TR 101 290 V1.4.1), within one time base |
| `pcr-release-timing` | hard | no more than `--release-pct-max` of intervals arrive further than `--release-ms` from the interval their own values assert, and accumulated drift stays within `--drift-ms`, being the standing lag the sender is allowed to hold; a sample below `--live-min-pcr` PCRs or `--live-cover-pct` of the window is a failure, not a pass (`--live` only) |
| `pcr-position` | shape | share of PCR packets within `--adjacent-packets` of the previous one |
| `pcr-schedule` | shape | share of PCR intervals whose bytes are within `--schedule-tolerance-pct` (default 1) or one packet of what `--mux-rate` implies (estimated from the capture if not given); hard, at that share, when `--schedule-pct-min` is given |

A stream carrying several PCR PIDs (one per program) is graded on the busiest,
since two correct grids offset from one another pool into one that neither keeps.
`sync` and `continuity` run only under `--live`: on a file, `compliance.py`
grades both through TSDuck's `tsanalyze`, which also catches a payload-less
packet advancing the counter. A pipe cannot go through TSDuck first without
rebuffering the arrivals `release` stamps.

Accumulated drift has two shapes and only one is a defect, so the check bounds
the total and reports the rate over the tail of the sample beside it. A sender
that buffers builds a standing lag once and then runs at the media rate: the lag
is a constant offset no receiver can see, and it cannot grow past the latency
budget the sender is allowed to hold, so set `--drift-ms` to that budget
(`export ts --max-age`, 500 ms by default). A pipe that is not running at the
media rate never stops accumulating and so breaches any fixed bound given a long
enough sample. The tail rate is what tells the two apart, and it needs a sample
longer than the lag takes to build: measured against the grid-sliced exporter,
the lag reaches ~480 ms over the first ~48 s and then holds to within 0.02 ms/s
over the following 40 s, whereas a 20 s window catches it still filling at
\~8.7 ms/s and cannot distinguish that from a slow pipe.

Two conditions are legal and must not be reported as defects. ISO 13818-1 2.4.3.4
lets a source declare a new time base by setting `discontinuity_indicator`, after
which the next PCR is a fresh value rather than the next point on the old ramp, so
the difference across that boundary measures nothing: the value and release checks
drop that one interval and count it separately, drift is summed over the intervals
actually graded, and the jump is not mistaken for a 33-bit wrap. Counting it read a
signalled splice as an 820 ms repetition breach and a continuity error at once.

Separately, "not measured" and "passed" are different answers and only one of them
belongs to a file. A file carries no arrival stamps, so release timing genuinely
cannot be graded and the check says so. Under `--live` the stamps were expected, and
a sample too small to mean anything is a truncated capture rather than a clean
stream, so it fails: without that, a producer which emitted two packets and exited
turned the gate green, and an exporter exiting early is common enough that the route
is a real one rather than a hypothetical.

`pcr-position` is a shape check because `export ts` is VBR by design. It is worth
reporting even so: a consumer holding only the byte stream, which is every
MPEG-TS tool, recovers the clock from where the PCR packets sit, so a layout
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

### Byte schedule

A census of the whole capture reports the average rate, and an exporter padding to
a declared rate gets that right. A receiver recovering its clock from packet
arrival needs something narrower: the rate over every PCR interval.
`pcr-schedule` takes consecutive PCRs on one PID and compares the bytes between
them with what the mux rate implies for the interval between their values,
reporting the relative error's p1, median, p99 and worst, and the share within
tolerance. PCRs from different PIDs are never pooled: two PIDs on offset grids
pool into a grid of half the interval that neither of them keeps.

One packet is always allowed. PCR packets sit on packet boundaries, so a mux whose
PCR values are on a time grid is up to a packet off at every interval even when its
schedule is exact, and below ~19 kB per interval one packet is more than 1 %. A
correct 2 Mb/s stream on a 25 ms grid reads −0.74 % and +2.27 % on alternate
intervals, which a percentage alone would fail.

Give the rate whenever it is known. The estimate is total bytes over total time,
so a transient biases every interval by the same amount: over a 20 s live window,
the exporter's unpadded first half-second pulled it ~3 % low and read every padded
interval after it as off schedule. `run.sh` passes the generated clip's rate
(`--bitrate`), and lets the grader estimate for `--source`.

The generated clip is a weak fixture for this check. It compresses to almost
nothing, so padding dominates and no keyframe outgrows its slot: measured against
the exporter this check was written for, 92-97 % of intervals were on schedule
over three 20 s runs, the misses being near-empty intervals from the unpadded
start. A real constant-rate capture is the case that
discriminates. A 60 s cut of a 9.95 Mb/s broadcast clip round-tripped through the
same harness came back with a median of 1,316 B between PCRs against 31,081 B
nominal and 3.1 % of intervals within tolerance, while its aggregate rate was
within 16 b/s of nominal:

```bash
just test ts --source cap.ts --duration 60 # reports the schedule, estimating the rate
./pcr-timing.py sub.ts --mux-rate 9945951 --schedule-pct-min 99 # gates on it
```

`--report-json <path>` writes the full report, and `--strict` promotes the shape
checks to hard.

## Table anchoring (`table-anchor.py`)

Every other check here grades one stream in isolation, and no single stream can
answer the question this one asks: **are a table's emission points a property of
the broadcast, or of the exporter that happened to emit them?**

It matters because two exporters of one broadcast are how redundancy is built. A
receiver merging two legs, or cutting from one to the other, needs them to agree
about where the tables sit. If each leg emits to its own clock the two outputs
are not interchangeable, and the divergence is permanent rather than something a
longer run settles.

One stream cannot show this because a timer and an anchor look identical from
inside: both produce a table every so often. Only a second leg, joining at a
different moment, separates them.

```bash
just test ts --pair                      # round-trip twice, grade the pair
just test ts --pair --pair-join 20 --duration 60   # join the second leg 20s in
./table-anchor.py a.ts b.ts              # grade two captures you already have
```

The second subscriber joins late on purpose. Two exporters started together can
agree on a cadence by having started together, which is the confound; a leg that
joins mid-broadcast has to derive its emission points from the media, because it
has no shared history to derive them from.

**What gets graded is the overlap, not the run**, so `--pair` defaults to a 45 s
duration rather than the 20 s the other arms use, and refuses a run leaving less
than 25 s of it. At 20 s with a 5 s join the legs share 15 s, which is about
seven SDT emissions against a floor of eight — the table this mode exists to
check would quietly drop to report-only. Raise `--duration` when you raise
`--pair-join`. `--pair` cannot be combined with `--live`: they grade different
things.

The measurement is the PTS of the frame each table was emitted against. `export
ts` writes the tables that are due and then the frame's PES packets into one
buffer, so the first PES header after a table gives the PTS of the frame that
triggered it. **Agreement** is the emission points both legs used over those
either used, counted only inside the media time the two captures share, so a
late join costs nothing. 100 % means the emission points are a function of the
broadcast.

| Option | Meaning |
|---|---|
| `--min-agreement` | percent of shared emission points required per table (default 90) |
| `--min-emissions` | a table with fewer emissions in the overlap is reported, not graded (default 8) |
| `--min-window` | seconds of shared media required before any verdict is given (default 20) |
| `--strict` | fail on report-only tables too |
| `--report-json` | write the full report |

`--min-window` exists because a capture that came up short is the commonest way
this grades clean: too little shared media puts the slower tables under the
emission floor, and a table reported without a verdict reads as a pass. The
window is taken from the media the two captures carry, **not** from the
emissions being scored — deriving it from the emissions is itself a false pass,
since a leg that stops emitting a table halfway through would pull the upper
bound back to its own last emission and score its desertion as 100 %.

Two things are deliberately not graded. A table seen on only one leg is reported
without a verdict, because that is a carriage question rather than an anchoring
one. TDT/TOT is report-only whatever its agreement, since it carries wall-clock
time and is *supposed* to track a clock rather than the media.

Where both legs run the same period at different phase, the report says so
rather than leaving a percentage to interpret:

```
  FAIL  SDT/BAT          10      10      19       1      5.26%
                    both legs emit every 2.000s, 0.480s out of phase
                    -> a timer started with the exporter, not an anchor in the media
```

## Open GOP (`open-gop.py`)

Broadcast contribution encoders commonly send open GOP: after the first IDR, each
keyframe is a non-IDR I picture flagged by a recovery-point SEI, and the B pictures
that follow it in decode order but are presented before it (its leading pictures)
reference the previous GOP. Only a viewer that already holds that GOP can decode
them. Dropping them is right at a tune-in, which the transport cannot see, so the
round-trip has to hand every one of them on, in decode order.

`--open-gop` swaps the generated clip for an x264 `open-gop=1` encode whose 24-frame
GOP and fixed B placement put three leading pictures behind every recovery point,
round-trips it, and runs `open-gop.py` on the source and capture before the
compliance report. With `--source`, it grades a real capture instead, and fails if
that capture has no leading pictures to grade.

```bash
./open-gop.py source.ts capture.ts
```

Access units are matched by their slice data, so the comparison ignores the
parameter sets and delimiters the round-trip may re-insert and the timestamp
rebase.

| Check | Severity | What it verifies |
|---|---|---|
| `fixture` | hard | the source has at least two non-IDR recovery points with leading pictures |
| `decode-order` | hard | the capture is one contiguous run of the source's access units from a random-access point, with DTS strictly increasing |
| `leading-pictures` | hard | every leading picture in that run is there and keeps the presentation offset the source gave it |
| `random-access-indicator` | shape | the exporter flags exactly the random-access access units |
| `recovery-point-sei` | shape | each recovery point keeps its SEI |
| `dts-before-pts` | shape | no access unit is stamped to decode after it presents |

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

`.github/workflows/interop.yml` runs `just test ts`, `just test ts --open-gop`,
`just test ts-eit`, and `just test ts-tstd` after the interop matrix (nightly, on
demand, and on PRs touching `test/ts/`).
`ts-eit` is `eit-roundtrip.sh`: it builds the sparse-schedule and
pending-version fixtures from a generated clip, round-trips them through a
relay, and censuses the capture, so a break in the generators or in the SI
carriage they pin fails a PR instead of landing silently. TSDuck comes from the
`nix develop` shell, so the run uses the same `tsp`/`tsanalyze` a local
developer would.

`.github/workflows/nightly.yml` runs `just test ts --live --duration 120` as
well. It stays off the PR path because release timing needs a real-time window to
measure: the grader has to sit on the pipe for the length of the capture, which
no per-PR gate should be paying for, and a scheduled arm on a quiet runner is a
better place to notice the exporter drifting off its own clock anyway. The window
is 120 s rather than the 20 s default because that is what the drift term costs
to read: under a minute it catches the mux buffer still filling and cannot tell
that from a pipe running slow.

## Caveats

- Physical-layer TR 101 290 items (RF, real sync-byte loss) cannot be measured
  from a file; TSDuck notes the same limitation.
- Wall-clock delivery jitter/burstiness is out of scope *for `compliance.py`*:
  all of its timing is derived from the stream's PCR, not from arrival times.
  `pcr-timing.py --live` covers that axis separately, by stamping a pipe.
