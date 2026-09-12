# [M] A Rust encoder adapts its bitrate to what its viewers report

## Goal

A `moq-video` or `moq-audio` encode producer given a feedback prefix
subscribes to the `.stats` broadcasts announced under it, reads the
per-broadcast subscriber track for the broadcast it publishes, and lowers its
bitrate while viewers report stalls and late frames, beside the congestion
estimate it already follows. Which viewers count is the publisher's choice of
prefix. Keyframe requests stay out.

## Plan

- `encode::Config` in `rs/moq-video` and `rs/moq-audio` gains
  `feedback: Option<feedback::Consumer>`, a handle built from an
  `origin::Consumer` and a prefix: it consumes announcements under the
  prefix, keeps one `moq_stats::Consumer<hang::Stats>` per `.stats` broadcast
  requesting `<own path>/subscriber.json`, and folds the reports into one
  signal, the share of viewers stalled over the last interval and their late
  frame rate, weighted equally per viewer. The counters are cumulative, so
  the handle keeps the previous snapshot per viewer and diffs it; a counter
  that goes backwards is a restarted viewer and resets that baseline. Viewers
  that stop reporting age out on the stats interval, so one stall long ago
  never lowers the target forever.
- `rate.rs` takes that signal beside the bandwidth estimate: a stalled share
  above a threshold steps the target down like a bandwidth drop, recovery
  follows the existing attack curve, and the estimate stays the ceiling.
  Audio follows the same signal with its narrower ladder.
- `moq import --feedback <prefix>` and `moq transcode --feedback <prefix>`
  wire it; each rung of the ladder reads its own broadcast's track.
- Test with the CLI publishing to a relay and two `moq play --stats` viewers
  under the prefix, one throttled through the impairment profile: the target
  bitrate drops within two intervals of the throttled viewer reporting stalls
  and recovers after it stops. Document the flag in `doc/bin/cli.md` and the
  loop in the moq-video README.

## Required

- [Schema and library](/quest/m2/qos/stats/schema.md) - the feedback it reads
- [Rust reporters](/quest/m2/qos/stats/rust.md) - the viewers that report and
  the CLI it wires

## Related

- [Ladder](/quest/m2/ladder/README.md) - the transcode ladder that adapts to
  its uplink today
- [Keyframe trigger](/quest/m2/keyframe-trigger.md) - the keyframe request
  this loop does not send
