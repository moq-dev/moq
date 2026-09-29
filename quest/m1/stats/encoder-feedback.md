# [M] A Rust encoder adapts its bitrate to what its viewers report

## Goal

A `moq-video` or `moq-audio` encode producer given a feedback handle solicits
feedback in its catalog. It reads the feedback track from every `.echo`
broadcast announced under the catalog's echo path, and lowers its bitrate
while viewers report stalls and late frames, beside the congestion estimate
it already follows. Keyframe requests stay out.

## Plan

- `encode::Options` in `rs/moq-video` and `rs/moq-audio` (the producer
  options, beside `bandwidth`) gains `echo: Option<echo::Consumer>`.
  - The handle is built from an `origin::Consumer` and the echo path,
    resolved against the publisher's broadcast.
  - It consumes `.echo` announcements under that path, subscribes to the
    feedback track on each, and reads its own renditions by alias.
  - It folds the reports into one signal: the share of viewers stalled over
    the last interval and their late frame rate, weighted equally per viewer.
  - The counters are cumulative, so the handle keeps the previous snapshot
    per viewer and diffs it. A counter that goes backwards means a restarted
    viewer and resets that baseline.
  - Viewers that stop reporting age out on the stats interval, so one stall
    long ago never lowers the target forever.
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
  drop, never below the floor; recovery follows the existing attack curve,
  and the estimate stays the ceiling. Audio does not follow its grant today
  (`Options::bandwidth` in `rs/moq-audio/src/encode/producer.rs` reserves
  only). It follows this signal only once the grant quest lands; until then
  the loop drives video.
- `moq import --echo <path>` and `moq transcode --echo <path>` set the
  catalog's `echo` section and wire the handle. Each rung of the ladder
  reads its own renditions.
- Test: the CLI publishes to a relay, and two `moq play --echo` viewers
  report under the echo path, one of them throttled through the impairment
  profile. The target bitrate drops within two intervals of the throttled
  viewer reporting stalls, and recovers after it stops. A unit test on mocked
  time checks that the quorum and floor bound one viewer's influence.
- Document the flag in `doc/bin/cli.md` and the loop and its config in the
  moq-video README.

## Required

- [Schema](/quest/m1/stats/schema.md) - the `echo` section and feedback snapshot
- [Rust reporters](/quest/m1/stats/rust.md) - the viewers that report and
  the CLI it wires

## Related

- [Ladder](/quest/m1/ladder/README.md) - the transcode ladder that adapts to
  its uplink today
- [Audio follows the grant](/quest/m1/2848-follow-the-bandwidth-grant-in-moq-audio-instead-of.md) -
  the audio rate follow this signal would feed
