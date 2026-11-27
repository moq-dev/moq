---
title: MPEG-TS
description: Carry a transport stream through MoQ, demultiplexed or whole, and what each mode guarantees
---

# MPEG-TS

`moq import ts` reads a transport stream on stdin and `moq export ts` writes one
to stdout. [SRT](/bin/srt) carries the semantic import and export over the
network; it has no passthrough mode.
There are two modes, and they make different promises.

```bash
# Semantic (default): demultiplex into tracks, rebuild a TS on the way out
moq --connect https://relay.example.com/anon --broadcast event.hang import ts < feed.ts
moq --connect https://relay.example.com/anon --broadcast event.hang export ts > out.ts

# Passthrough: carry the multiplex whole, write the same packets back out
moq ... --broadcast event.hang import ts --passthrough < feed.ts
moq ... --broadcast event.hang export ts --passthrough > out.ts
```

## Choose a mode

| | Semantic | Passthrough (`--passthrough`) |
| --- | --- | --- |
| On the wire | One track per elementary stream, plus the PSI and SI the import keeps, in the [`mpegts`](/draft/moq-mpegts) catalog section | One [passthrough](/concept/hang#transport-streams) track of whole 188-byte packets, named in the same section |
| Output | A rebuilt TS: same media, programs, descriptors and kept SI; new packetization, PCR and table cadence | The source's packets, byte for byte, from the first group on |
| Browsers and `moq play` | Watch the video and AAC or Opus audio; MP2, AC-3 and E-AC-3 play only where the player has a decoder for them, which `moq play` does not | Have nothing to decode |
| A relay | Drops and prioritizes per track | Caches and drops whole runs of packets |
| Scrambled services, CA, unlisted PIDs | Refused and counted | Carried |
| Multi-program | One program per broadcast (`--program`) | The whole MPTS in one broadcast |
| Loss | Video resumes at its next keyframe; the clock and tables stay valid | Surviving packets go out unchanged; the gap stays a gap |
| Buffer conformance | Rebuilt to the [T-STD](#evidence) for the supported codecs | Exactly as conformant as the source |

Use semantic unless you need the exact packets: a scrambled service, a stream
the importer refuses, or downstream gear that must see the source's own
multiplex. Byte identity is not the semantic mode's contract.

For a source that is already MPEG-TS, pipe its bytes rather than re-muxing:
`ffmpeg -c copy -f mpegts` drops PIDs (NIT, TDT/TOT, extra audio, teletext,
SCTE-35). For example,
`tsp -I ip 239.1.2.3:1234 -O file - | moq ... import ts`.

## Semantic mode

### What it carries

The import parses H.264/H.265 video and AAC (ADTS), MP2, AC-3, E-AC-3, and
Opus audio into renditions, without transcoding: a player still needs a
decoder for each codec. Everything else it keeps rides through whole:

| Source | Carried as |
| --- | --- |
| H.264, H.265 | Video renditions, with each PID and its descriptors |
| AAC (ADTS), MP2, AC-3, E-AC-3, Opus | Audio renditions. The export labels Opus mono, stereo, or 3 to 8 channels in Vorbis order, and refuses other mapping families |
| MPEG-2 video, LATM AAC, KLV and other metadata, DVB AC-3, subtitles, any other PES | Whole PES at their place on the program clock (PCR), PTS and DTS rebased. MPEG-2 video still paces the program, and a subscriber joins it at a sequence header |
| SCTE-35, DSM-CC, private sections | Whole sections; SCTE-35 `pts_adjustment` rebased |
| DVB SI on PIDs 0x0010, 0x0011, 0x0012, 0x0014 | A snapshot track per PID and `table_id`; pending (`current_next_indicator` 0) sections are dropped |
| Program identity, program and ES descriptors | Recorded in the catalog and followed through each PMT revision |

It refuses, with a warning and a `refused` count in the
[stats](/concept/stats#mpeg-ts-sections): a scrambled stream; PES carrying an
ESCR, pack header or TREF; a non-video PES with no length or a malformed
header; `0x86` without a SCTE-35 registration, which may be DTS audio; and any
stream in a program with no PCR. A PID no table claims, such as the CAT, conditional access or ATSC
PSIP, is dropped with one warning and counted the same way.

H.264/H.265 access units can span PES, and one PES can hold several. Each
access unit needs a PTS, or a declared proportional POC clock: AVC needs
`fixed_frame_rate_flag` and `temporal_poc_flag` in the TS timing descriptor
plus fixed-rate SPS VUI, and HEVC needs VUI's
`vui_poc_proportional_to_timing_flag`. Missing timing ends the import rather
than repeat a timestamp. Sections and whole PES take their place from the
media clock, or from the first PCR when the PMT declares only data; one before
the first PCR is dropped. Audio-only and data-only programs work too; a
data-only program still needs a PCR. A program declaring media waits for its
first timestamp and is refused if the input ends or breaks its clock first.
PES and untimed sections share a bounded staging budget, so a source that never
supplies a PES boundary or a clock fails rather than grow without bound.

### Programs and changes

A broadcast holds one program. A multi-program stream is refused until
`--program 2` picks one, or `--program all` publishes each as its own
broadcast (`event.hang` becomes `event/1.hang`, `event/2.hang`), following the
PAT: a program it adds publishes, one it drops ends, and one that returns
publishes as a new [identity](/concept/moq-lite#publisher-identities). A listed
program whose PMT does not arrive within 0.5 s of program clock counts as
`PMT_error_2` in the feed health while the others publish, and starts once its
PMT arrives.

A PMT that adds, removes, or retypes a stream, or reuses a PID, ends the old
track and continues under a fresh name; the export announces each layout at its
next PMT `version_number`. A PAT that moves the PMT is followed, and the export
moves its own PMT behind a new PAT.

The import, over stdin or SRT, republishes as a new identity when the source moves onto another
timeline: the PCR and every stream stepping back, flagged or not, a step more
than 10 s forward without `discontinuity_indicator`, or a PAT naming another
`transport_stream_id` or program number. The new stream carries nothing over
from the old PAT and PMT, so it announces once the source repeats them and its
next keyframe arrives, up to a GOP later; it keeps the measured mux rate.
`export ts --stitch --follow` carries on into it (see
[retention](/bin/cli#retention-and-latency)). An unflagged step usually means
redundant encoders behind a changeover switch: time-lock their clocks, or have
the switch set `discontinuity_indicator`. A flagged jump forward continues the
broadcast and is flagged on the exported PCR. `--program all` still ends at a
rewind or a changed identity. Timestamps follow their history across the 33-bit
wrap (every 26.5 hours), so a jump of more than half a wrap reads as a rewind.

### Export

The export rebuilds PES headers, packets, the PCR (on the first video PID, else
audio, else a PCR-only PID), and PAT/PMT, every 100 ms while media flows and at
each keyframe, with each SI table at half its DVB maximum, so the gaps an analyser
measures stay inside TR 101 290's limits. Padded, a copy adds no packets while
the same tables are still in the receiver's 1,536-byte system buffer, so frequent
keyframes cost nothing extra, and PAT and PMT of more than two packets together
repeat only on the 500 ms and keyframe cadence, since a revision must wait for a
repeat still in that buffer. It pads to the source's constant mux rate when the
catalog recorded one, plus 60,160 b/s (a packet every 25 ms) for its own clock
and PES framing, since a source such as FFmpeg's default audio-only output has
no null packets to make room for them. Below 50 ms of `--delay`, audio can't
go out a slot ahead, so a padded AAC export runs at least at the rate each slot
needs for two AAC frames. Or it pads to `--mux-rate`, which is used as given, on a schedule an IRD or groomer can lock to, with each PCR within 500 ns
of its byte position. A variable-rate source exports unpadded, with no mux-rate
or system-buffer claim.

Each frame goes out as early as the receiver's buffers admit, up to `--delay`
(default 1 s) ahead of its decode time, so the output trails the source by
`--delay`. Audio's decoder buffer fills no faster than the multiplex, so a slow
padded multiplex sends audio up to half of `--delay` ahead, spreading it across
slots, and a source that sends its audio only at its decode time adds that much
latency. A frame that cannot arrive in time at the rate fails the export, as
does video whose HRD declares a longer buffer delay than `--delay`; the error
names the delay it needs.

## Passthrough mode

`import ts --passthrough` publishes every packet, in input order, on one
track. It reads the PAT and PMT to find the clock and the video stream and
nothing else past the adaptation field, so a scrambled multiplex groups and
paces like its clear twin. It takes the whole stream, SPTS or MPTS: it never
extracts a program, so it refuses `--program`. To carry programs separately,
import each as its own SPTS into its own broadcast. 192- and 204-byte packets
are refused rather than reframed. `--pcr-pid` paces on another PID's PCR.

Each frame is a run of packets that ends at the next PCR on the clock PID or at
the next group start, stamped with the PCR time of its first byte. A group
starts at a `random_access_indicator` on the video PID (or the clock PID with
no video), at a PCR clock break, after a second without either, or where the
next frame would take the group past the relay's cache bound. A flagged PCR
jump forward carries its step into the timestamps. A step back, flagged or
not, or an unflagged one more than 10 s forward, such as a switch between two
unlocked encoders, is spliced instead: the timestamps carry on from where the
clock was, and the bytes still go out unchanged, step and all.

`export ts --passthrough` writes each run `--delay` after its time on the
source clock, spread evenly up to the next run's time seven packets (a UDP
datagram) at a time, so the output keeps the source's rate and two exporters
that each receive every run before it comes due emit the same bytes. A run
whose successor has not arrived when it comes due, follows a loss,
or is more than 100 ms on, goes out whole, so the timing matches between
exporters only where each had the successor: a `--delay` longer than the
arrival jitter by a PCR interval. Runs that come due while stdout blocks go
out together once it drains, so a stalled reader sees a burst whatever the
delay. It takes no `--mux-rate`.
`--follow` and `--stitch` carry it across the broadcast's returns and
replacements: a restarted import is a new publisher identity, so `--stitch`
writes its packets verbatim from the first group its live subscription delivers,
after what the old one delivered, with no overlap window. The seam shows as each PID's
continuity break, as a splice in the source would. Only the hang catalog names the
track, so an `.msf` broadcast needs `--catalog-format hang`. The encoding is in
[draft-lcurley-moq-mpegts](/draft/moq-mpegts).

## Acquisition

An importer joining a live feed waits for what it needs before publishing.

- **Semantic:** the import waits for the PAT, PMT, and a PCR, and publishes the
  catalog with the first media. A constant mux rate follows as a catalog update
  once two seconds of PCR have measured it; a variable-rate source never records
  one. An export without `--mux-rate` waits up to those two seconds for the rate
  before it starts, so a variable-rate source costs it one wait. A subscriber
  joins at a keyframe, and the export puts a PAT and PMT ahead of it. From
  there a late exporter writes the same packets as a running one, but for
  continuity counters.
- **Passthrough:** nothing is published until the PAT, the clock program's
  PMT, a PCR, and a group start. Bytes before that, and bytes off the 188-byte
  grid, are dropped: the output equals the input over the *admitted interval*,
  from the first group on, not the whole input. An exporter that joins later
  starts at a later group.

A group boundary is where MoQ can start delivery; it is not a promise that a
decoder can start there. The catalog's `randomAccess` is true only while the
PAT lists one program with at most one video stream and every group so far
began at a `random_access_indicator`. In an MPTS the programs' random access
points rarely line up, so a group usually starts mid-GOP for some of them, and
passthrough never inserts tables to help. Even a true `randomAccess` says
nothing about whether the PAT, PMT, codec configuration, or clock reference has
arrived yet; a receiver acquires them from the packets that follow.

## Loss

Loss here means MoQ content that never arrives: a group skipped or expired at a
relay, a group cut short, or a frame later than `--delay`. QUIC retransmits
what the network drops, so random packet loss alone loses no content.

- **Semantic:** a missing group leaves a hole in the media, not in the
  multiplex: padding keeps the PCR on its continuous grid, unflagged. Only a
  hole longer than the export can pad over skips PCR slots, and then the first
  PCR after them flags the clock jump. PAT/PMT versions do not change either
  way. A group cut short keeps what arrived. Video resumes at its next
  keyframe, dropping H.264 and H.265 leading pictures that reference the group
  before; MPEG-2 video goes out whole. Other tracks carry on. On a path slower
  than the media, whole groups are shed and the latency holds. After an outage that exhausts the buffered
  media, overdue padding can arrive in a burst when media resumes.
- **Passthrough:** surviving packets go out unchanged, on their source schedule.
  A missing interval stays missing: the export never repairs continuity counters,
  inserts tables or stuffing, or closes the gap. Each skip counts in the
  `release` section's `gaps` and each late run in `dropped`
  ([stats](/concept/stats#mpeg-ts-sections)). Exact surviving bytes do not mean
  the output decodes, or meets the T-STD, across the gap.

Damage in the source is the importer's concern, not MoQ's. In semantic mode a
lost packet, or one the receiver flagged with the transport error indicator, is
dropped on its own PID, so video freezes until its next keyframe while the
other tracks carry on, and feed health (stalled streams, TR 101 290 counters)
is logged and reported without changing what publishes. A media unit, PES
header, or adaptation field that arrives whole but does not parse ends the
import with the cause: with neither loss nor the flag to explain it, the muxer
wrote it wrong. A packet or PES scrambled by conditional access is refused and
counted, and its stream resumes once it is clear, video at its next keyframe.
Passthrough carries damaged packets as they came.

## Evidence

Every claim above has an automated test. **PR** runs in the merge queue's Rust
gate (`just rs test -p moq-mux`, on a paused clock). **Nightly** runs in the
[TS workflow](https://github.com/moq-dev/moq/blob/main/.github/workflows/ts.yml)
every night: a relay round trip through the CLI, graded by TSDuck, FFmpeg, and
the harness's own models. **Nightly only** marks that workflow's long,
real-time runs. The [harness README](https://github.com/moq-dev/moq/blob/main/test/ts/README.md)
details each grader and its negative controls.

### Semantic profile

| Claim | Test | Runs |
| --- | --- | --- |
| H.264 and AAC decode to the source's frames and samples | `just test ts` ([decode.py](https://github.com/moq-dev/moq/blob/main/test/ts/decode.py)), `just test ts-controls` | Nightly |
| H.264/H.265 survive repartitioned PES and POC-derived timing | [`import.rs`](https://github.com/moq-dev/moq/blob/main/rs/moq-mux/src/container/ts/import.rs) `video_poc_repartition_preserves_frames_timestamps_and_export` | PR |
| MP2, AC-3, E-AC-3 round-trip byte-exact | [`export_test.rs`](https://github.com/moq-dev/moq/blob/main/rs/moq-mux/src/container/ts/export_test.rs) `mp2_kyrion_roundtrip_byte_exact`, `ac3_roundtrip_byte_exact`, `eac3_roundtrip_byte_exact` | PR |
| Opus layouts export, and an unlabelled head refuses | [`export_test.rs`](https://github.com/moq-dev/moq/blob/main/rs/moq-mux/src/container/ts/export_test.rs) `opus_export_import_roundtrip`, `opus_export_refuses_a_head_it_cannot_label` | PR |
| Whole PES (KLV, DVB AC-3) come back byte-equal outside PTS/DTS, at their placement | `just test ts --klv --strict`; [`pes_packet_test.rs`](https://github.com/moq-dev/moq/blob/main/rs/moq-mux/src/container/ts/pes_packet_test.rs) | Nightly, PR |
| MPEG-2 video decodes to the source's pictures | `just test ts --mpeg2`; [`routes_test.rs`](https://github.com/moq-dev/moq/blob/main/rs/moq-mux/src/container/ts/routes_test.rs) `mpeg2_video_rides_whole_and_survives_a_round_trip` | Nightly, PR |
| SCTE-35 and DSM-CC sections round-trip | [`export_test.rs`](https://github.com/moq-dev/moq/blob/main/rs/moq-mux/src/container/ts/export_test.rs) `scte35_fixtures_survive_roundtrip`; [`routes_test.rs`](https://github.com/moq-dev/moq/blob/main/rs/moq-mux/src/container/ts/routes_test.rs) `dsmcc_types_ride_as_sections` | PR |
| Refusals: PES clock fields, scrambling, ambiguous `0x86`, unclaimed PIDs | [`pes_packet_test.rs`](https://github.com/moq-dev/moq/blob/main/rs/moq-mux/src/container/ts/pes_packet_test.rs) `a_pes_with_a_field_no_rebase_reaches_refuses_its_stream`; [`routes_test.rs`](https://github.com/moq-dev/moq/blob/main/rs/moq-mux/src/container/ts/routes_test.rs) `ambiguous_streams_are_refused_and_latm_rides_whole`, `unclaimed_pids_count_once_the_pmt_is_in`; [`import.rs`](https://github.com/moq-dev/moq/blob/main/rs/moq-mux/src/container/ts/import.rs) `scrambled_pes_is_refused_until_clear` | PR |
| DVB SI survives, on its own interval, pending versions dropped | [`export_test.rs`](https://github.com/moq-dev/moq/blob/main/rs/moq-mux/src/container/ts/export_test.rs) `service_layer_survives_roundtrip`, `si_pids_are_re_emitted_on_their_own_interval`; `just test ts-eit` | PR, Nightly |
| PMT revisions: streams joining and leaving, retypes, PID reuse | `just test ts --revision --strict`; [`revision_test.rs`](https://github.com/moq-dev/moq/blob/main/rs/moq-mux/src/container/ts/revision_test.rs) | Nightly, PR |
| PAT changes: moved PMT, programs added and dropped | [`revision_test.rs`](https://github.com/moq-dev/moq/blob/main/rs/moq-mux/src/container/ts/revision_test.rs) `a_moved_pmt_moves_behind_a_new_pat`; [`programs.rs`](https://github.com/moq-dev/moq/blob/main/rs/moq-mux/src/container/ts/programs.rs) `programs_follow_the_pat` | PR |
| Programs without video | `just test ts --clockless audio-scte35`, `sections`, `pes` | Nightly |
| Timeline steps republish; a changed identity restarts at its PAT | [`step_test.rs`](https://github.com/moq-dev/moq/blob/main/rs/moq-mux/src/container/ts/step_test.rs) | PR |
| Transport structure: PAT/PMT, CRC, continuity, PCR presence and order | `just test ts`, `--fragment-pes` (TSDuck) | Nightly |
| T-STD and system buffers, at 2 and 10 Mb/s, a 9 Mbit CPB, a full leak, a full and an over-Rx multiplex, open GOP | `just test ts`, `--bitrate 2000000`, `--hrd`, `--leak`, `--full`, `--headroom`, `--open-gop --strict`; model controls `just test ts-tstd` | Nightly |
| CBR padding, PCR within 500 ns, byte schedule | `just test ts` (`pcrverify`, `pcr-schedule`); [`export_test.rs`](https://github.com/moq-dev/moq/blob/main/rs/moq-mux/src/container/ts/export_test.rs) `export_pcr_is_its_byte_position_at_the_rate` | Nightly, PR |
| Release timing and PCR rate against the arrival clock | `just test ts --live --duration 120` | Nightly only |

### Passthrough

| Claim | Test | Runs |
| --- | --- | --- |
| The capture is the source in place over the admitted interval, with the source's compliance verdicts | `just test ts --passthrough`; `just test ts-passthrough-capture` (a hardware encoder's capture with a dirty start) | Nightly |
| A whole MPTS, two programs on their own clocks | `just test ts --passthrough --mpts` | Nightly |
| An import killed and restarted under a running export costs no more than its downtime and the restarted import's first group | `just test ts --passthrough --restart 10`; [`release_test.rs`](https://github.com/moq-dev/moq/blob/main/rs/moq-mux/src/container/ts/release_test.rs) `a_stitched_release_carries_on_into_a_restarted_import`, `a_stitched_release_carries_on_into_a_rewound_clock`, `the_same_identity_returning_resumes_the_release` | Nightly, PR |
| Byte identity from the first group; nothing before PMT and PCR; off-grid junk dropped | [`passthrough_test.rs`](https://github.com/moq-dev/moq/blob/main/rs/moq-mux/src/container/ts/passthrough_test.rs) `a_broadcast_capture_is_byte_identical_from_the_first_group`, `nothing_is_published_before_the_pmt_and_a_pcr`, `leading_junk_does_not_hide_the_grid` | PR |
| Scrambled input groups and paces like its clear twin | [`passthrough_test.rs`](https://github.com/moq-dev/moq/blob/main/rs/moq-mux/src/container/ts/passthrough_test.rs) `a_scrambled_twin_groups_and_paces_the_same` | PR |
| `randomAccess` false for several programs or video streams | [`passthrough_test.rs`](https://github.com/moq-dev/moq/blob/main/rs/moq-mux/src/container/ts/passthrough_test.rs) `two_programs_on_one_clock_publish_no_random_access`, `two_video_streams_in_one_program_publish_no_random_access` | PR |
| Clock: wrap, flagged jump, steps spliced, 192/204-byte packets refused | [`passthrough_test.rs`](https://github.com/moq-dev/moq/blob/main/rs/moq-mux/src/container/ts/passthrough_test.rs) | PR |
| Export releases each run byte-identical on its own PCR, whatever the delivery skew while each run arrives before it comes due, spread across its PCR interval where the next is buffered within 100 ms | [`release_test.rs`](https://github.com/moq-dev/moq/blob/main/rs/moq-mux/src/container/ts/release_test.rs) `a_capture_is_released_byte_identical_on_its_own_pcr`, `releases_fed_with_different_skew_write_the_same_bytes`, `an_object_is_spread_across_its_pcr_interval`, `runs_written_in_one_burst_fail_the_grade` | PR |

### Acquisition and loss

| Claim | Test | Runs |
| --- | --- | --- |
| Semantic: a join starts on a whole group, each acquisition point carries PAT and PMT and decodes like the continuous source | [`recovery_test.rs`](https://github.com/moq-dev/moq/blob/main/rs/moq-mux/src/container/ts/recovery_test.rs) `a_join_starts_on_a_whole_group`, `a_late_catalog_starts_on_the_group_in_flight` | PR |
| Semantic: a late exporter matches a running one | [`export_test.rs`](https://github.com/moq-dev/moq/blob/main/rs/moq-mux/src/container/ts/export_test.rs) `late_join_matches_a_running_exporter`; `just test ts --pair` | PR, Nightly only |
| Semantic: a padded hole keeps the PCR unflagged; a longer one flags only the first PCR after its skipped slots; tables keep their versions | [`export_test.rs`](https://github.com/moq-dev/moq/blob/main/rs/moq-mux/src/container/ts/export_test.rs) `a_skipped_group_flags_the_clock`; [`recovery_test.rs`](https://github.com/moq-dev/moq/blob/main/rs/moq-mux/src/container/ts/recovery_test.rs) `disjoint_outages_across_both_tracks_keep_the_clock` | PR |
| Semantic: skipped, cut, stalled, and late groups, decoded against the source; open GOP and HEVC RASL trimmed, RADL kept | [`recovery_test.rs`](https://github.com/moq-dev/moq/blob/main/rs/moq-mux/src/container/ts/recovery_test.rs) (graders with failure controls) | PR |
| Semantic: two minutes of faults hold their latency | `just rs ts-recovery` | Nightly only |
| Semantic: a congested and an overflowing path shed whole groups, stay T-STD compliant, keep their latency | `just test ts --headroom --rate 7000000`, with `--queue 250ms` | Nightly only |
| Passthrough: a missing group leaves its time out and counts one gap; a cut group loses only its tail; a late run is dropped alone | [`release_test.rs`](https://github.com/moq-dev/moq/blob/main/rs/moq-mux/src/container/ts/release_test.rs) `a_group_never_delivered_leaves_its_time_out`, `a_group_cut_short_loses_only_its_tail`, `an_object_past_its_deadline_is_a_gap_in_that_release_only` | PR |
| Passthrough: an overflowing path keeps surviving packets in place through the source's last | `just test ts --passthrough --rate 7000000 --queue 250ms` | Nightly only |
| Source damage drops only the damaged PID's unit | [`import.rs`](https://github.com/moq-dev/moq/blob/main/rs/moq-mux/src/container/ts/import.rs) `damaged_tei_recovers`, `video_drops_a_partial_access_unit_across_a_continuity_break` | PR |
| A whole unit or header that does not parse ends the import | [`import.rs`](https://github.com/moq-dev/moq/blob/main/rs/moq-mux/src/container/ts/import.rs) `undecodable_nal_ends_the_import`, `malformed_opus_pes_ends_the_import_unpublished`, `malformed_headers_end_the_import`, `invalid_adaptation_fields_end_the_import_on_media_and_clock_pids` | PR |

### Not claimed

These are outside what the tests above establish:

- **Hardware receivers and commercial analyzers.** No IRD or commercial T-STD
  analyzer runs in CI; passing establishes the software profiles above, not
  broadcast compliance in general.
- **Buffer models for other codecs.** The T-STD model has no parameters for
  MPEG-1 video, MPEG-4 part 2, or HEVC beyond Main and Main 10, and refuses
  them. Opus borrows ADTS's buffer size. Sections and private data are not
  buffer-graded.
- **Decoded content beyond H.264, H.265, AAC and MPEG-2 video.** MP2, AC-3,
  E-AC-3 and Opus are compared byte for byte, not decoded.
- **Variable-rate sources.** No PCR accuracy, byte-schedule, or system-buffer
  claim for unpadded output.
- **`--program all` through a relay.** Covered by the Rust tests only; no
  harness round trip.
- **Programs carried separately in passthrough.** Each is an ordinary SPTS
  import; no test combines them.
- **Random datagram loss.** `just test ts --loss` exists but no CI step runs
  it; the congested paths above grade realistic loss instead.
- **Physical-layer TR 101 290 items**, which a file cannot show.
