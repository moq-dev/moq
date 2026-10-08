# [M] A Rust encoder adapts its bitrate to what its viewers report

## Goal

A `moq-video` or `moq-audio` encode producer given a feedback handle solicits
feedback in its catalog. It reads the feedback track from every `.echo`
broadcast announced under the catalog's echo path, and lowers its bitrate
while viewers report stalls and late frames, beside the congestion estimate
it already follows. Keyframe requests stay out.

## Plan

Deferred in the 2026-09-30 audit and moved to m3 in the 2026-10-05 audit: no named consumer; the media stats it
reads land first in m1.

Decided 2026-10-08: parked, and the transcode leg is out. The
[ladder controller](/quest/m3/ladder/controller.md) owns every
`moq-transcode` rung's target, so `moq transcode --echo` feeds the folded
stall signal to that controller rather than to `rate::Control`; split it
into its own quest requiring the controller if this is revived. This quest
covers the encode producers and `moq import --echo`, which need no ladder.

- `encode::Options` in `rs/moq-video` and `rs/moq-audio` (the producer
  options, beside `bandwidth`) gains `echo: Option<echo::Consumer>`.
  - The handle is built from an `origin::Consumer` and the echo path,
    resolved against the publisher's broadcast.
  - It consumes `.echo` announcements under that path, subscribes to the
    feedback track on each, and reads its own renditions by ID.
  - It folds the reports into one signal: the share of viewers stalled over
    the last interval and their late frame rate, weighted equally per viewer.
  - The counters are cumulative, so the handle keeps the previous snapshot
    per viewer and diffs it. A counter that goes backwards means a restarted
    viewer and resets that baseline.
  - A viewer counts toward a rendition while its latest snapshot has a row
    for that ID, so viewers of other rungs, or ones that switched away,
    never dilute the share. The announcement, not the age of its last
    report, keeps that snapshot current: an unchanged snapshot sends no frame, so a quiet healthy
    viewer must not age out. Diffing already makes one stall long
    ago contribute nothing to later intervals.
- Trust (2026-09-29): the token prefix is the boundary. Any viewer the
  application's tokens let publish under the echo path counts. How far
  viewers may move the target is application-specific (a simulcast ladder
  suffers less from one viewer than a single rendition), so the step-down
  policy is built in but takes a tunable config struct: a minimum viewer
  count or quorum, a bitrate floor, and the stalled-share threshold, with a
  way to disable it. Reason: a safe default without hard-coding one
  application's policy.
- `moq_mux::rate::Control` takes that signal beside the bandwidth estimate.
  A stalled share above the threshold steps the target down like a bandwidth
  drop, never below the floor; recovery follows the existing decay ramp,
  and the estimate stays the ceiling. Audio does not follow its grant today
  (`Options::bandwidth` in `rs/moq-audio/src/encode/producer.rs` reserves
  only). It follows this signal only once the grant quest lands; until then
  the loop drives video.
- `moq import --echo <path>` sets the catalog's `echo` section and wires the
  handle.
- Test: the CLI publishes to a relay, and two `moq play --echo` viewers
  report under the echo path, one of them throttled through the impairment
  profile. The target bitrate drops within two intervals of the throttled
  viewer reporting stalls, and recovers after it stops. A unit test on mocked
  time checks that the quorum and floor bound one viewer's influence.
- A benchmark sweeps viewers and renditions per snapshot, runs at least
  nightly, and shows one interval's fold growing with the rows it reads, not
  a full-table scan per rendition.
- Document the flag in `doc/bin/cli.md` and the loop and its config in the
  moq-video README.

Open, to settle before starting (moved from the
[media stats](/quest/m1/stats/README.md) line):

- **Referenced-rendition feedback.** A derivative catalog (a `moq-transcode`
  passthrough) collects feedback for a source rendition it lists, but owns
  no encoder for it, and the source encoder reads only its own catalog's
  prefix. Candidates: the derivative forwards those rows to the source's
  echo path, or the source encoder also reads catalogs that reference it,
  or referenced renditions stay report-only.
- **Shared echo prefixes.** Two catalogs can resolve their echo paths to one
  prefix (`../viewers` from `room/a/live` and `room/b/live`). Then a viewer
  using one name for both closes one `.echo` with the other, and each
  publisher reads the other's reports under a shared ID. Candidates:
  require each catalog's echo prefix to be its own, as application policy
  like the token rights, or carry the catalog's broadcast in the snapshot
  and ignore reports for another.

## Required

- [Schema](/quest/m1/stats/schema.md) - the `echo` section and feedback snapshot
- [Rust reporters](/quest/m1/stats/rust.md) - the viewers that report and
  the CLI it wires

## Related

- [Ladder controller](/quest/m3/ladder/controller.md) - owns the rung targets a split-out transcode leg would feed
- [Audio follows the grant](/quest/m1/2848-follow-the-bandwidth-grant-in-moq-audio-instead-of.md) -
  the audio rate follow this signal would feed
- [Ladder](/quest/m3/ladder/README.md) - the transcode ladder, which today
  encodes every rung at a fixed rate with no bandwidth input
