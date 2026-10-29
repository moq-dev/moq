# TS / IRD compliance test

Validates the MPEG-TS that the moq subscriber emits (`moq ... export ts`) against
what an Integrated Receiver/Decoder (IRD) expects. It round-trips a stream
through a relay (`import ts` -> relay -> `export ts`), captures the output, and
runs [TSDuck](https://tsduck.io) plus a custom analyzer over it.

The harness keeps software decoding, transport structure, decoder-buffer models,
and release timing as separate assertions. Passing them establishes the named
software profiles below; it does not certify a hardware IRD or every TS layout.

- `decode.py` runs strict software FFmpeg decoding and compares captured content
  with the source for generated closed-GOP H.264/AAC-LC streams.
- `compliance.py` uses TSDuck for structure and a custom elementary-stream T-STD
  model for supported profiles.
- `accuracy.py` requires measured PCRs within 500 ns of their CBR byte position.
- `pcr-timing.py` checks PCR values and byte schedules from files, and release
  timing from a live pipe.
- `table-anchor.py` compares table emission points between two subscribers.
- `open-gop.py` checks the coded access units and leading pictures of open GOPs.
- `overhead.py` counts the packets a capture spends on clocks and audio framing
  against its source.
- `holes.py` finds media missing from a generated clip's capture, from its
  presentation timestamps against the source's. The export derives each group's
  decode timestamps from that group alone, so they step at group boundaries with
  nothing lost.
- `passthrough.py` places every packet of an `export ts --passthrough` capture
  at its source position and compares the compliance verdicts.

## Running

```bash
just test ts-controls           # real FFmpeg/TSDuck positive and negative controls
just test ts-tstd               # buffer-model controls
just test ts                    # generate a clip, round-trip, analyze
just test ts --source cap.ts    # round-trip a real capture instead
just test ts --analyze-only x.ts # skip the round-trip, analyze a file
just test ts --strict           # also fail on broadcast-shape warnings
just test ts --with-eit         # add a synthetic EPG first, report which SI survived
just test ts --live             # grade PCR release timing off the live pipe
just test ts --pair             # two exporters of one broadcast, grade table anchoring
just test ts --open-gop         # open-GOP clip; orphan leading pictures are trimmed
just test ts --hrd              # 1080p video filling a broadcast-sized 9 Mbit CPB
just test ts --headroom         # the same with four audio PIDs, muxed above the video's Rx
just test ts --leak             # 1080p video with no HRD, filling its level's 24 Mb/s leak
just test ts --full             # the same with two audio PIDs, filling its multiplex with no nulls
just test ts --fragment-pes    # legal headers split across packets, with FFmpeg/TSDuck grading
just test ts --klv              # KLV and DVB AC-3 carried as whole PES, graded against the source
just test ts --mpeg2            # MPEG-2 video carried as whole PES, decoded from the capture
just test ts --revision         # the PMT revised mid-stream, a KLV stream joining and leaving
just test ts --delay 2s         # pass the exporter's --delay
just test ts --hrd --rate 7000000 # congest the subscriber's leg below the media rate
just test ts --loss 1           # drop 1% of the relay's datagrams to the subscriber
just test ts --passthrough      # carry the multiplex whole, grade it packet by packet
just test ts --passthrough --mpts # the same with two programs on their own clocks
just test ts-passthrough-capture # the same over an in-repo capture the T-STD model passes
```

`--fragment-pes` shifts generated audio/video PES bursts into their following
null slots after splitting the header. It cycles through header-byte boundaries,
preserves packet count, PCR positions, payload bytes, and PID continuity, then
runs the same independent decode/content and transport checks as the default arm.

`--klv` muxes DVB AC-3 beside the AAC, several sync frames a PES, and `klv.py`
fills null slots with asynchronous KLV (`stream_type` 0x06, `KLVA` registration, no
PTS) and synchronous KLV (`stream_type` 0x15, `metadata_stream` 0xFC, a PTS per
picture). The importer decodes none of them, so each PES rides its track whole.
`klv.py grade` holds each KLV stream in the capture to its source: the same PID,
`stream_type` and descriptors; every PES from the join to the end, in order and
byte-equal outside its PTS and DTS, which move by one shift; none out before its
source placement on the program clock, and none after its PTS; and FFmpeg reading
each as KLV with the same PES and PTS. `holes.py` and the T-STD model grade the
AC-3, whose buffer drains each sync frame at its own decode time.

`--mpeg2` publishes Main Profile at Main Level MPEG-2 video beside MPEG audio. The
importer does not decode MPEG-2 video, so it rides its track whole while its PTS
still paces the program. `mpeg2.py` decodes source and capture with FFmpeg: past
the GOP the subscriber joins in, every picture is the source's next one, through
the source's last. `holes.py` and the T-STD model grade the rest.
`--revision` has `revision.py` add asynchronous KLV to the default clip's null
slots from a third of its program clock to two thirds, and revise the PMT at each
end: the next `version_number` lists the stream before its first packet, and the
one after drops it once its last packet is out. `revision.py grade` holds the
capture's PMTs to the source's layouts in order, each at the version after the one
before and the KLV entry byte-equal, every packet of the stream between the PMT
listing it and the one dropping it, and every PES of the stream in order and
byte-equal.

`--live` swaps the analyzer, not the rig: the same round-trip runs, but the
subscriber's stdout goes straight into `pcr-timing.py` instead of a capture file.
That is the only arm that can see release timing at all, and it is what nightly
runs (see [CI](#ci)).

The default arm runs `pcr-timing.py` over its capture too, after `compliance.py`,
for the PCR checks `compliance.py` leaves to it: the value interval (hard), and
whether the bytes between consecutive PCRs are the ones the mux rate implies
([`pcr-schedule`](#byte-schedule), which gates whenever the rate is known, as the
generated clip's is). With the rate known it also runs TSDuck's `pcrverify` from
the first PCR, failing on any PCR more than 500 ns off its byte position
at the rate: the `PCR_accuracy_error` an IRD or a TR 101 290 probe reports, which
`pcr-schedule`'s packet of slack lets through.

Publisher and captured subscriber processes must exit successfully. Captures drain
through EOF within the existing process deadline; a nonempty partial file cannot
hide a failed pipeline or timeout. The live arm also requires its grader to pass;
after its measurement window, the reader drains through source EOF so neither a
broken pipe nor another exporter failure needs to be ignored.

`--analyze-only` needs only TSDuck + Python, so you can point it at any captured
subscriber output:

```bash
moq --connect http://localhost:4443 --broadcast live.hang export ts > sub.ts
./run.sh --analyze-only sub.ts
```

Requirements: `tsp`, `tsanalyze` and `tstables` (TSDuck) and `python3` for every mode; the
round-trip modes also need `cargo`, `ffmpeg`, `ffprobe`, `curl`, and `timeout`.
`just test ts-tstd` also needs `ffmpeg` (with libx264) and `ffprobe` for its generated
references.

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
| `pmt-versions` | hard | each program's PMT changes only at a new `version_number`, and never returns to an older one |
| `psi-crc` | hard | no section dropped for a bad CRC |
| `continuity` | hard | no continuity-counter discontinuities |
| `pcr-presence` | hard | every program's declared PCR PID carries PCR (0x1FFF declares none) |
| `pcr-monotonic` | hard | PCR strictly increases (one 33-bit wrap tolerated), except into a PCR that signals `discontinuity_indicator` |
| `duration-fidelity` | hard | exported PCR span tracks the source's duration (round-trip only) |
| `service-descriptors` | shape | an SDT naming the service is present |
| `tstd` | hard | the supported elementary-stream T-STD models: no TB, MB, EB or B overflow, no access unit incomplete at its decoding time (see [T-STD](#t-std)) |
| `system-buffers` | hard | each program's TBsys and Bsys: neither overflows, and TBsys empties at least once a second (see [T-STD](#t-std)) |

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

`tstd` models supported audio and video streams using the ISO 13818-1 system target
decoder (2.4.2; Rec. ITU-T H.222.0, whose 10/2014 edition is a free download),
fed on the stream's own PCR clock:

```text
video (H.262 2.4.2.3, AVC 2.14.3.1, HEVC 2.17.2)   TB --Rx--> MB --Rbx (leak)--> EB --DTS--> decoder
audio (2.4.2.3)                                    TB --Rx--> B  ----------------PTS--> decoder
```

It fails a stream where TB, MB, EB or B overflows, where TB stays occupied for a
second, where an access unit is not wholly in EB/B at its decoding time
(underflow), or where a byte waits longer than the STD delay bound (1 s, 10 s for
AVC/HEVC). The report gives each stream's peak fill per buffer, how late the worst
underflowed access unit finished arriving, and the longest any access unit waited.

TSDuck supplies the transport analysis, not this decoder-buffer model. Commercial
analyzers advertise T-STD checks; no suitable maintained open-source replacement
was established for this harness. Its parameters are transcribed from the specs: H.262 Tables 8-13 and 8-14, H.264 Table A-1 and H.265 Table A.8 for the level, the
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

MPEG-2 video takes Rx = 1.2 Rmax from its level, Rbx = Rmax at Low and Main level
and min(1.05 Res, Rmax) at High-1440 and High, EB from the sequence header's
`vbv_buffer_size`, and MB from what the level's VBV leaves over, as H.222.0 2.4.2.3
specifies for the leak method. A stream whose `STD_descriptor` clears `leak_valid`
while its pictures code a `vbv_delay` other than 0xFFFF takes the `vbv_delay`
method instead, which is not modelled, so it is refused. Its picture, GOP or
sequence header opens each access unit.

Opus is graded against ADTS's buffers for the same channel count: the Opus-in-TS
draft gives Rx (2 Mb/s for 1-2 channels, matching ADTS) but leaves the buffer size
unset, so that size is borrowed rather than specified. A stream with no parameters
at all is refused by name rather than skipped, which fails the check: MPEG-1/2
video, DVB E-AC-3, and HEVC beyond Main/Main 10. Sections and private data
(SCTE-35, teletext) are listed as not graded; passing elementary-stream buffers
does not establish conformance for them.

`system-buffers` runs each program's PIDs 0-3 and PMT through one TBsys (512 B at
Rxsys = 1 Mb/s) and on into one Bsys (1,536 B, emptied at Rsys = max(80 kb/s,
transport rate / 500)), on the program's PCR, as 2.4.2.3 specifies: a packet's
bytes enter Bsys as they leave TBsys, and a duplicate (2.4.3.3, the same bytes
but for a refreshed PCR or OPCR) enters TBsys but not Bsys. Each program is
followed through the PAT and its PMTs, as a receiver would: its packets on the PMT
PID then in force count, timed on the PCR PID then in force, and both buffers keep
their bytes when either PID moves, until a complete PAT drops the program. Across a
PCR PID move the last PCR on the old PID and the first on the new are the program's
successive PCRs, so unless the move signals a discontinuity they must increase and lie
within 0.1 s (2.7.2). Rsys drains Bsys at the transport rate each pair
of PCRs gives, so a rate change takes effect where it happens. A system packet on a
stretch of the PCR PID with fewer than two PCRs fails, since nothing checked it;
a program declaring no PCR (0x1FFF) has no clock to model. The
exporter paces its tables against both buffers only when it pads to a
multiplex rate. Unpadded output makes no system-buffer claim: its tables go out
as muxed. A signalled PCR discontinuity starts fresh buffers, since the timestamps
on either side of it are on different clocks.

Every packet on an elementary stream's PID enters TB, including adaptation-only
ones (a PCR, stuffing) and legal duplicates (2.4.3.3: the same counter and
payload twice); only PES bytes of a first copy go on to MB/B. Video access units
are read from the ES rather than taken from PES boundaries: one opens at the first
delimiter, parameter set or SEI after the previous picture's slices (H.264
7.4.1.2.3, H.265 7.4.2.4.4), and H.222.0 requires a delimiter in each. A PES may
carry several, but only the AU starting in it takes its timestamp once. The model
can derive later presentation times from anchored POC and explicitly proportional
VUI timing; AVC also needs the TS timing descriptor's fixed-rate and temporal-POC
flags. Its frame-only profile supports AVC POC types 0/2 and HEVC without sub-picture
HRD. Inferred decode times additionally need each picture's HRD DPB output delay;
PTS is never substituted for DTS. Missing signaling, field/pic-struct timing, AVC
POC type 1 or MMCO5 with missing timestamps are refused. Video decode times must
strictly increase.

Audio bytes enter B as they leave TB, so a frame that ends partway through a packet
is complete once its own last byte has left, not the packet's, and B is checked
just before each removal as well as after each packet. Video keeps one
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

No restamp of the Kyrion capture can overflow its video MB: it holds the level's
whole CPB less the declared one, about 3.6 MB, more than the 4 s capture carries.
An 8 s FFmpeg AVC Baseline@2.0 clip has no declared NAL HRD, so its MB uses the
much smaller level defaults. It passes as encoded; restamping only its PCRs at
4x overflows MB (and TB), closing the separate MB negative-control gap.

Generated AVC and HEVC controls put two AUs in each PES, preserving every ES
byte's original arrival position. Proportional POC timing and HRD picture timing
make both pass; removing HRD picture timing makes both refuse their unknown decode
times. Their no-reordering and B-picture variants exercise separate PTS and DTS.

The packet layouts a capture cannot be edited into are built synthetically: a
10 Mb/s single-video stream carrying the Kyrion SPS, one access unit per PES, with
a PCR packet between them. Each case fails without the handling it names:

| Case | Expected |
|---|---|
| as built | pass |
| two access units in one PES without a declared clock | refused |
| four adaptation-only packets after an access unit | TB overflow |
| an access unit's first packet sent twice | pass |
| the PMT declares a PCR PID that carries no PCR | `pcr-presence` fails |
| a PAT and three PMT copies back to back | TBsys overflow |
| ten PMT copies, each arriving as TBsys passes the last on | Bsys overflow |
| PMT copies at Rxsys for 1.5 s, one packet ahead | TBsys not emptied within 1 s, Bsys overflow |
| six PMT copies sent twice, the second with a refreshed OPCR | pass |
| the PAT moves the PMT, then ten PMT copies on its new PID | Bsys overflow |

A tables-only stream fills Bsys at 100 Mb/s and sends three more PMT copies once
its rate falls to 25 Mb/s, where Rsys is 80 kb/s rather than 200: Bsys overflow.

Synthetic MPEG audio streams pack several 576-byte frames per PES, so frames end
partway through packets, and the last is cut off by the end of the capture:

| Case | Expected |
|---|---|
| four frames per PES, the first decoded 0.3 ms after its last byte leaves TB, before the rest of that packet has | pass |
| seven frames per PES, the first decoded as B passes its size partway through a packet | B overflow |

A generated AVC/AAC reference also passes: stereo 48 kHz AAC at 128 kb/s,
`-muxdelay 0.1 -muxpreload 0.1 -pes_payload_size 0`, in a 2 Mb/s TS. The short
lead keeps the 3,584-byte AAC buffer below capacity, and the mux rate keeps
audio packet bursts within the TB drain. Every elementary stream must have
access units to grade; the builder also checks that AAC is actually present.

The 10 Mb/s FFmpeg clip `run.sh` generates remains unsuitable as a positive
control: its default 0.7 s audio lead overflows B, and even at 0.1 s a
four-packet audio burst at that transport rate overflows TB. These references
close the named MB/AAC gaps, not every codec, buffer or packet layout.

### Gate

`tstd` is a hard check: `just test ts` fails on any buffer overflow or underflow.
`export ts` passes it since its jitter buffer and constant-rate schedule (#4645).
A stream with no audio or video to model, or no access unit inside a modelled time
base, only warns, since there is nothing to grade.

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

From a pipe, `pcr-rate` also grades the PCR clock's rate against the arrival
clock, as the least-squares slope over each time base after the start-up third:
within 30 ppm (`--rate-ppm`), the tolerance ISO 13818-1 2.4.2.1 gives a system
clock. `release` cannot see a clock running at a steady wrong rate, which keeps
its intervals and only adds to the drift. At 30 ppm a 77 s window moves only
2.3 ms, so the fit excludes two arrival effects that are not the clock. Reads only
ever land late, so it fits the least-delayed PCR in each second, the floor that
scheduling and pipe delay sit on; a live control's mean read delay wandered by
2.4 ms while that floor held within 0.7 ms. And it breaks wherever that floor
steps by more than the `release` tolerance from one second to the next, pooling
the runs either side: a stall the sender caught up from leaves a standing lag,
which a single line reads as a rate (70 ms mid-sample is over 1,000 ppm), though
it is the bounded lag `release` already grades. A clock off rate moves the floor
by microseconds a second, so it never steps and stays measured.

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
internally consistent, so absolute rate is not what this grades, except against
the arrival clock in `pcr-rate`. `pcr-schedule` is the other exception when it is
given `--mux-rate`, which pins the rate the way `duration-fidelity` does; without
it, it estimates the rate from the capture and grades only how evenly the bytes
are laid over the PCRs.

| Check | Severity | What it verifies |
|---|---|---|
| `sync` | hard | no invalid sync bytes / transport-error packets (`--live` only) |
| `continuity` | hard | no discontinuities, and a payload-less packet must not advance the counter (ISO 13818-1 2.4.3.3) (`--live` only) |
| `pcr-value-interval` | hard | no interval above `--repetition-ms` (default 100, TR 101 290 V1.4.1), within one time base |
| `pcr-release-timing` | hard | no more than `--release-pct-max` of intervals arrive further than `--release-ms` from the interval their own values assert, and accumulated drift stays within `--drift-ms`, being the standing lag the sender is allowed to hold; a sample below `--live-min-pcr` PCRs or `--live-cover-pct` of the window is a failure, not a pass (`--live` only) |
| `pcr-rate` | hard | the PCR rate fitted against each second's least-delayed read stays within `--rate-ppm` (default 30), after excluding the first third, separating declared time bases and breaking where that floor steps by more than `--release-ms` (`--live` only) |
| `pcr-position` | shape | share of PCR packets within `--adjacent-packets` of the previous one |
| `pcr-schedule` | shape | share of PCR intervals whose bytes are within `--schedule-tolerance-pct` (default 1) or one packet of what `--mux-rate` implies (estimated from the capture if not given); hard, at that share, when `--schedule-pct-min` is given |

A stream carrying several PCR PIDs (one per program) is graded on the busiest,
since two correct grids offset from one another pool into one that neither keeps.
`sync` and `continuity` run only under `--live`: on a file, `compliance.py`
grades both through TSDuck's `tsanalyze`, which also catches a payload-less
packet advancing the counter. A pipe cannot go through TSDuck first without
rebuffering the arrivals `release` stamps.

Live runs write `pcr-timing.json` beside `timing.out`. Failures retain their run;
set `HARNESS_KEEP=1` to retain passing controls too. The JSON report keeps the
graded PID's raw `[packet_index, unwrapped_pcr_ticks, arrival_seconds, pid]` samples,
the indices that begin new time bases, wrap count, and grading parameters. PCR ticks
are at 27 MHz; arrivals use the reader's `time.monotonic()` clock and are upper
bounds on when the exporter released those bytes. These preserve the inputs to
the rate, release and byte-position checks so their verdicts can be replayed.
Arrival delays alone do not establish whether a source clock, exporter, pipe or
reader caused the discrepancy.

Accumulated drift has two shapes and only one is a defect, so the check bounds
the total and reports the rate over the tail of the sample beside it. A sender
that buffers builds a standing lag once and then runs at the media rate: the lag
is a constant offset no receiver can see, and it cannot grow past the latency
budget the sender is allowed to hold, so set `--drift-ms` to that budget
(`export ts --delay`, 1 s by default). A pipe that is not running at the
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
so a transient biases every interval by the same amount. `run.sh` passes the generated
clip's rate (`--bitrate`) with `--schedule-pct-min 99`, and lets the grader estimate,
and only report, for `--source`.

Every PCR interval is graded. Import holds the catalog until its initial two-second
PCR window has measured a stable rate or established that the input is variable-rate.
A constant-rate exporter therefore has the rate before it sends its first PCR.

At the default 10 Mb/s the generated clip compresses to almost nothing, so padding
dominates and no keyframe outgrows its 31 kB slot. At 2 Mb/s its 13-19 kB keyframes
outgrow a 6 kB slot, so the exporter has to spread each over the slots before its
DTS; CI runs that too (`just test ts --bitrate 2000000`). `--hrd` goes further: a
1080p encode with a 9 Mbit NAL HRD kept near full by noise, the shape of a
contribution encoder's output, which sends pictures most of a second ahead of their
decode time and fills the decoder buffer to its declared size at the default delay (the recipe
moq-dev/moq#4645 graded with; CI runs it too). Every other clip muxes at or below the
video's Rx, where its transport buffer cannot fill. `--headroom` puts that video
beside three MPEG-1 Layer II PIDs and an AC-3 one in a 12.5 Mb/s multiplex, above
the video's 10.8 Mb/s Rx, as a broadcast feed runs; each slot then has to
interleave the video with the audio and the nulls, or TB overflows
(moq-dev/moq#5142; CI runs it too). A real constant-rate
capture discriminates further. A 60 s cut of a 9.95 Mb/s broadcast clip
round-tripped through the harness before the export kept a schedule came back with
a median of 1,316 B between PCRs against 31,081 B nominal and 3.1 % of intervals
within tolerance, while its aggregate rate was within 16 b/s of nominal:

```bash
just test ts --source cap.ts --duration 60 # reports the schedule, estimating the rate
./pcr-timing.py sub.ts --mux-rate 9945951 --schedule-pct-min 99 # gates on it
```

`--report-json <path>` writes the full report, and `--strict` promotes the shape
checks to hard.

## Table anchoring (`table-anchor.py`)

Two exporters of the same broadcast must converge after acquisition. `--pair`
starts the second subscriber five seconds late and checks table placement on the
shared PCR timeline. This measures table alignment; it does not establish packet
identity or ST 2022-7 compatibility.

```bash
just test ts --pair
just test ts --pair --pair-join 20 --duration 60
just test ts-pair-controls
./table-anchor.py a.ts b.ts --report-json pair.json
```

The CBR slot scheduler interleaves tables and PES, so the next PES timestamp does not
identify when a table was sent. The grader interpolates each table's packet
position between PCR samples. A match allows one packet of placement quantization
plus one PCR tick of rounding. Matching is one-to-one, so duplicates count.

The comparison starts at the later leg's **second video random-access PES**,
after its first acquired group, matching the exporter's late-join convergence
regression. This boundary comes from media markers, never from the tables under
test or a fixed time allowance. The end is the earlier leg's final PCR. Tables
that disappear cannot shorten their own scoring window. Captures with multiple
program clocks, variable bitrate, midstream PCR discontinuities, missing acquisition markers, or
broken packet framing are refused. PCR wrap is supported.

The default run lasts 45 seconds and requires 20 seconds of acquired shared
clock. PAT, declared PMTs, and every observed non-clock table must each have at
least eight emissions **on both legs**, with at least 90% agreement. Missing or
sparse tables fail rather than quietly becoming report-only. TDT/TOT remains
informational because its content follows wall-clock time; `--strict` grades it
as well. `--source` also requires the source's SI tables and PAT-declared PMTs, catching loss on both legs. These checks do not diagnose a process timer from cadence alone.

| Option | Meaning |
|---|---|
| `--min-agreement` | Required percentage of matched emissions (default 90) |
| `--min-emissions` | Required emissions per leg and table (default 8) |
| `--min-window` | Required seconds of acquired shared clock (default 20) |
| `--source` | Require the source's SI tables on both legs |
| `--strict` | Grade TDT/TOT too |
| `--report-json` | Save the window, tolerance, counts, and verdicts |

`just test ts-pair-controls` runs deterministic transport fixtures through the
actual grader. Identical streams, different PES interleave, late acquisition with
bootstrap tables, and PCR wrap must pass. Persistent phase shifts, accumulating
drift, disappearing/absent/sparse tables, and insufficient shared media must fail.
Controls run nightly and on release PRs; the real pair runs nightly and on manual TS validation.

## Open GOP (`open-gop.py`)

`just test harness` also runs the actual JS container and player against generated
open-GOP access units with Chromium's software WebCodecs decoder forced. It
checks a join, continuous GOPs, and a later delivery hole; disabling the trim
reproduces `EncodingError` instead of silently accepting orphan references.

Broadcast contribution encoders commonly send open GOP: after the first IDR, each
keyframe is a non-IDR I picture flagged by a recovery-point SEI, and the B pictures
that follow it in decode order but are presented before it (its leading pictures)
reference the previous GOP. Only a viewer that already holds that GOP can decode
them. The consumer trims the recovery group's orphan leading pictures at a join
or after a delivery gap. Continuous groups keep every picture in decode order.
A declared end is a fresh encoder boundary, not a delivery gap.

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
| `decode-order` | hard | each delivered segment starts at a random-access point, omits only its orphan leading pictures, and keeps DTS strictly increasing |
| `leading-pictures` | hard | every continuous leading picture is kept with its source presentation offset; recovery-group orphans are absent |
| `random-access-indicator` | shape | the exporter flags exactly the random-access access units |
| `recovery-point-sei` | shape | each recovery point keeps its SEI |
| `dts-before-pts` | shape | no access unit is stamped to decode after it presents |

## Programs without video

`just test ts --clockless audio-scte35`, `--clockless sections`, and
`--clockless pes` generate an audio/SCTE-35 program or a data-only program.
The audio profile carries PCR on its audio PID; the data-only profiles use a
PCR-only PID with no elementary-stream entry. These nightly arms run the same
TSDuck compliance and PCR schedule checks, then compare every captured data
unit and its placement with an exact source suffix through EOF. Audio content
is not decoded in this profile. Rust tests also cover PCR wrap, media joining
a data-only clock later, and refusal when a data-only PMT names no PCR.

## Mux overhead (`overhead.py`)

A source that fills its multiplex with no null packets leaves the export no room
to spend more than it does on PES headers, part-filled audio packets, and PCRs
outside media packets (moq-dev/moq#5362). `--full` exports at 900 ms, above its
source's lead of about 0.65 s, which absorbs a small excess for a while, so it also
runs `overhead.py` on the source and capture before the compliance report.

ffmpeg pads its multiplex with nulls, so `drop_nulls.py` removes them before the PCRs
are restamped at the rate the rest fill. Restamping alone would move each packet by the
nulls' uneven spread before it, tens of milliseconds, and the DVB AC-3 the export carries
at its source placement would arrive after it decodes. So every PID but the video keeps
its time in the padded clip, and the video, sent most of a second ahead, absorbs the rest.

```bash
./overhead.py source.ts capture.ts
```

It counts the packets on each audio PID and the PCR-only packets on each PID,
scaling the capture's counts by how much of the source's elementary stream on the
PID it carries, and fails when their total exceeds the source's.

## Impaired path (`--rate`, `--loss`)

`--rate` and `--loss` put [`moq-shaper`](../../rs/moq-shaper/README.md) between
the relay and the subscriber, with 25 ms each way. Unlike `netem` in two network
namespaces, as moq-dev/moq#4613 built it, the shaper needs no privileges and
leaves the host's network alone. The run prints the shaper's counters beside the
exporter's `dropped` (missed their deadline), `trimmed` (before the output began)
and `gaps` (skipped upstream).

### Congested path (`--rate`, `--queue`)

`--rate <bits/s>` carries the relay's datagrams to the subscriber at that rate
behind a queue `--queue` deep (2 s by default), and tail-drops what overflows it:
bufferbloat, which is how real paths fail. Below the media's rate the path cannot
carry it all, so the run judges what the export does instead. It must stay T-STD
conformant and continuous (`compliance.py`, `pcr-timing.py` and PCR accuracy gate
as on a clean path), shed whole groups rather than stall, and hold its latency
rather than let it grow with the queue. Nothing bounds how much it sheds.

The generated clips go through `holes.py --shed`, which matches the capture's
frames to the source's by presentation timestamp, since the export derives each
picture's decode timestamp and a group cut short moves it. Verbatim MP2, AC-3
and E-AC-3 timestamps are compared after their exact legacy microsecond wire
conversion back to 90 kHz, rather than allowing an arbitrary timestamp error.
Of each source group,
the capture keeps a prefix in decode order (all, part, or none) and never resumes
mid-group, so it also joins on a sync frame, and a frame reordered, repeated, or
not in the source fails. The source's last group must arrive whole: a stall, or a latency grown
past the exporter's 1 s delay, costs it. The decoded-content comparison owes every
unit, so it does not run, and `--open-gop`, which owes every leading picture, is
refused. A passthrough grades its packets in place and reports its gaps
(`passthrough.py --loss-ok`), but must still end on the source's last packet.

The run fails unless the rate limit held datagrams back and the export shed
something: a rate above the media's never narrows the path.

The default queue is twice the export's delay, so a sender that kept it full
would make every unit late. The delay-based default (BBRv3) does not: it keeps
about a round trip queued, so the backlog lands at the relay, which ages groups out
(`Old`), and the queue itself overflows only when it is about a round trip deep.
The loss-based `MOQ_QUIC_CONGESTION_CONTROL=loss` (CUBIC) fills it, latency
outgrows the delay, and the export refuses the run as a clock drift.

So the queue's own tail drop runs as a lane of its own: CUBIC behind a 250 ms
queue, five round trips deep, which it overflows on every sawtooth, while a
retransmit behind a full queue (about twice 300 ms) still lands inside the 1 s
delay. A loss-based run fails unless the queue overflowed.

```bash
MOQ_QUIC_CONGESTION_CONTROL=loss just test ts --headroom --rate 7000000 --queue 250ms
```

Measured on 2026-10-18 and 2026-10-19 with other test runs sharing the host:

| Run | Queue | Duration | Result |
|---|---:|---:|---|
| `--hrd --rate 7000000` | 2 s | 120 s | passes; 37.6 s of video shed in whole groups, none overflowed |
| `--hrd --rate 7000000` | 150 ms - 1 s | 40-120 s | none overflowed |
| `--hrd --rate 7000000` | 100 ms | 40-120 s, six runs | 0, 0, 0, 3, 5 and 28 overflowed |
| `--hrd --rate 7000000` | 75 ms | 60 s, two runs | 70 and 103 overflowed |
| `--hrd --rate 7000000` | 50 ms | 60-120 s, two runs | graders pass; 111 and 269 overflowed |
| `--hrd --rate 3500000` | 2 s | 60 s | fails: none overflowed, 56 s of video shed and the last group lost |
| `--hrd --rate 7000000 --live` | 2 s | 60 s | passes; 99.8 % of PCRs released on time, drift -28 ms |
| `--passthrough --rate 7000000` | 2 s | 120 s | passes; its gaps leave 463 continuity errors (reported) |
| `--hrd --rate 7000000`, CUBIC | 250 ms | 120 s, four runs | passes; 423-430 overflowed, 18-42 units missed their deadline, 36.5-39.9 s shed |
| `--hrd --rate 7000000`, CUBIC | 500 ms | 60-120 s, three runs | passes; 541-625 overflowed, 79-229 units missed their deadline |
| `--hrd --rate 7000000`, CUBIC | 1 s | 120 s | fails: 919 overflowed, 107 s shed and the last group lost |
| `--hrd --rate 7000000`, CUBIC | 2 s | 60 s | fails: 918 overflowed, then the clock-drift refusal |
| `--headroom --rate 7000000`, BBRv3 | 2 s | 120 s, saved capture regraded | passes; none overflowed, 62.5 s video and 3 AC-3 frames shed, no MP2 holes |
| `--headroom --rate 7000000`, CUBIC | 250 ms | 120 s | passes; 427 overflowed, 74 units missed their deadline, 64.5 s video and 3 audio frames shed |
| `--passthrough --rate 7000000`, CUBIC | 250 ms | 120 s | passes; 428 overflowed, 116 gaps, no missing tail; 458 continuity errors reported |

```bash
./holes.py capture.ts source.ts [--shed]
```

Without `--shed`, `holes.py` grades a path that should lose nothing: a capture's
step past one and a half of its source's is a hole, and every source stream must
be in the capture and end within half a frame of the source's. Each stream of a
generated clip keeps one frame step, so it reads the capture's timestamps against
the source's, independently of what the exporter reports. Either way a stream may
start up to 3 s late: the subscriber joins at the relay's latest group once the
catalog is out, which skips the first 1-2 s on a clean path. Omitting MoQ content
on purpose is graded on a paused clock in
`rs/moq-mux/src/container/ts/recovery_test.rs`: each arm republishes a generated
clip under a fault schedule (a group skipped or cut, a track or the video held
past its deadline, a join, a late catalog, A/V skew, the PTS wrap, a resolution
change, a declared PCR discontinuity or its lost marker, H.264 open GOP, HEVC
RASL and RADL, and a bottleneck below the media rate behind a FIFO twice the
delay deep that tail-drops), grades the output against the ledger of what was
delivered and when, and decodes it with FFmpeg from each point a receiver
acquires at, against the continuous source decode. Each of those points must
carry its own PAT and PMT, sent after the picture before it. Its graders' failure
paths run beside it, and `just rs ts-recovery` runs two minutes of the same faults
nightly, failing if the latency drifts and keeping a failing arm's export and
ledgers under `$HARNESS_RUNS/ts-recovery/`.

### Lossy path (`--loss`)

`--loss <percent>` drops that share of the relay's datagrams at random.
`TSC_SEED` (default 1) fixes which, not when the rest arrive, and the run fails
if the shaper dropped none. Real paths do not lose datagrams independently of
their load, so this proves correctness only: QUIC retransmits what the path drops,
so a capture with a hole or a gap fails as on a clean path. It does not judge the
export's quality or latency, and no CI step runs it. It cannot be combined with
`--rate` or `--live`. Use `--rate` without random loss when grading quality or
release timing on an impaired path.

## Passthrough (`--passthrough`, `passthrough.py`)

`--passthrough` publishes with `import ts --passthrough` and captures with
`export ts --passthrough`, so the capture should be the source's own packets.
`passthrough.py` grades it against the source alone:

- The admitted interval is the source from the capture's first packet to the
  end. What comes before it is the acquisition prefix, which the importer
  withholds until its first group, plus whatever the export joined after; the
  grader reports its size and does not count it as loss.
- Every captured packet must be the source packet at its exact position in that
  interval, null packets, tables, unknown PIDs and adaptation bytes included.
  A position is only claimed for a run of 16 matching packets, so a duplicated
  null or table packet fails rather than passing as an identical later one.
  Packets the capture skips are gaps, reported by position and PID.
- On a complete capture, every `compliance.py` check (TSDuck's structure, the
  T-STD and system buffer models) must give the capture the verdict it gives
  the admitted interval. The export carries whatever the source is, so a source
  that fails a check must fail it the same way: the generated clip's AAC
  overflows its T-STD buffer on its own, and the Kyrion capture CI also runs
  passes. `decode.py` then compares the generated clip's decoded content.

`--mpts` generates two programs with their own PCR PIDs and keyframes every 25
and 40 frames, so the importer's groups, which open on program 1's random
access points, mostly open mid-GOP for program 2. The capture is still the
source in place: a group boundary promises no immediate decode for every
program, and passthrough neither inserts tables nor extracts a program.

A gap fails the run, `--loss` included, since QUIC retransmits what the path
drops. A path that loses content, as `--rate` does, runs the grader with
`--loss-ok`: gaps are reported and the checks run on the capture alone, since a
gap can take part of a PES or the next table with it, and the source's packets
in place keep their continuity counters across it. The capture must still end on
the source's last packet, which a stall or a growing latency loses.

```bash
./passthrough.py source.ts capture.ts [--loss-ok]
```

Release timing is the Rust tests' job
(`rs/moq-mux/src/container/ts/release_test.rs`, on a paused clock), with the
packet and schedule oracle in `oracle.rs` and the fixtures it cannot get from a
capture in `fixture.rs`.

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

## Decoded content (`decode.py`)

The generated closed-GOP H.264/AAC-LC profile, at both 10 and 2 Mb/s, fully
decodes every expected video/audio stream with software FFmpeg. Missing streams,
empty output, decoder errors (including error logs with a zero exit status), and
configuration changes fail. Video frame hashes must match an intact source
suffix through EOF; frame timestamps must keep one common rebase.

Audio timestamps must remain contiguous, allowing the 90 kHz PTS quantization
and FFmpeg's conversion to the sample clock. Audio sample counts pin the admitted
source offset against the same video rebase, so a shifted audio track or a lost
tail fails. Exact PCM comparison uses a fresh decode of source AAC starting at
the same access unit: AAC overlap and perceptual-noise state differ between a
cold decoder and a decoder that saw earlier frames. No arbitrary warmup samples
are discarded. PES boundaries and TS packetization may change.

The suffix is the exporter's to choose, and it starts on a keyframe: a join
whose start group went stale before anything of it went out starts at the next
one. Anything lost after the first frame out fails, a group cut short included. When it does, the harness prints what the
exporter reported: the `TS export release clock` counts (`dropped` late,
`trimmed` before the output began, `gaps` skipped upstream) and each loss it
logged.

`just test ts-controls` exercises unchanged input, remuxing with new timestamps
and PES sizes, timestamp quantization, and a cold join. Negative controls require
the intended failure for missing audio, changed pictures, changed audio, a lost
tail, A/V skew, video clock drift, an audio gap, corrupted coded payload, and
empty input. Real TSDuck controls remove PAT, break continuity, set TEI, and check
PCR against a wrong rate or an empty measurement. Process controls also require
nonzero/timeout exit codes to fail despite nonempty output. A missing tool or
an unexpected failure category is a failing test, not a successful negative.

Other codecs, raw carriage, and arbitrary source captures do not yet have
decoded-content comparisons; H.264 and HEVC loss, recovery and open-GOP
acquisition have them in `recovery_test.rs`. Their existing
specific checks remain active and the harness explicitly reports the narrower
coverage. VBR and independent-clock MPTS need applicable timing models before
claiming the CBR PCR-accuracy profile.

## CI

The round-trip rig explicitly negotiates `moq-lite-07-wip` on the relay,
publisher and subscribers. Its publisher epochs allow acquisition to continue
when a session is replaced. Production defaults remain unchanged: Lite-06
cannot prove publisher identity across session replacement and must refuse a
pending acquisition. `rs/moq-net/tests/integration/catalog_acquisition.rs` covers both
outcomes deterministically in Rust CI, without retries or implicit `--stitch`.

[`.github/workflows/ts.yml`](../../.github/workflows/ts.yml) is independent of
browser interop. It runs nightly on `main`, and release PRs touching a workspace
crate `run.sh` builds (kept in sync by `sh/gh/ts-paths.py`), the harness, or the
toolchain run it too:
`just test ts-controls`, `just test ts-pair-controls`, the default and 2 Mb/s round trips, HRD, leak, full, headroom,
open-GOP, KLV (strict), MPEG-2 video, PMT revision, passthrough (generated, Kyrion and MPTS), EIT, and T-STD controls. Each step runs even after another test fails.
All recipes use the same pinned Nix FFmpeg and TSDuck locally and in CI.

Nightly and manual runs also invoke `just test ts --pair`,
`just test ts --live --duration 120`, `just test ts --headroom --rate 7000000 --duration 120`,
the same with `--queue 250ms` and `MOQ_QUIC_CONGESTION_CONTROL=loss`,
and `just test ts --passthrough --rate 7000000 --queue 250ms --duration 120`
with `MOQ_QUIC_CONGESTION_CONTROL=loss`, and `just rs ts-recovery`.
The pair exercises late-joining subscribers; the live window distinguishes ongoing
clock drift from initial buffer filling; the congested paths judge what the export
does when the path cannot carry the media.
These are actual test invocations, not optional tools installed on a runner.

Failed runs retain source/capture files, process logs and statuses, tool versions,
decoder commands, frame hashes, PCM, and grader reports under the harness run
directory. CI uploads that directory for seven days. `HARNESS_KEEP=1` preserves
successful local evidence too. `ts-eit` retains its sparse-schedule and pending
version fixtures; `ts-tstd` retains its existing model controls.

## Caveats

- Physical-layer TR 101 290 items (RF, real sync-byte loss) cannot be measured
  from a file; TSDuck notes the same limitation.
- Wall-clock delivery jitter/burstiness is out of scope *for `compliance.py`*:
  all of its timing is derived from the stream's PCR, not from arrival times.
  `pcr-timing.py --live` covers that axis separately, by stamping a pipe.
