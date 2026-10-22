# [L] Native publishers disable simulcast rungs the grant cannot fund

## Goal

A publisher encoding several video renditions of one source stops encoding
its top renditions when its bandwidth grant cannot fund them, advertising
each as `enabled: false` in the catalog, and re-enables them with hysteresis
once the grant recovers. The lower renditions keep their share instead of
every rung degrading together. Covers the Rust and FFI publishers (the
`rs/moq-video` producer under the `rs/moq-mux` rate policy) and the OBS
plugin; the JS publisher is
[JS rung disable](/quest/m2/probe/rung-disable-js.md). `@moq/watch`,
`moq play`, `moqsrc`, WHEP, HLS and DASH export, and the moq-mux exports
already skip disabled renditions and follow one that flips; moq-ffi and moq-c
refuse to subscribe one and leave the fallback pick to the application.

## Plan

Today each video encoder reserves its configured ceiling and follows its own
grant down (`rs/moq-video/src/encode/producer.rs`), so a congested uplink
squeezes every rendition at once and a top rung starved of most of its share
keeps encoding.

Decided with the maintainer on 2026-10-20: the native publishers moved to m1
and require the session send target (a session probes up to its
`bandwidth::Allocator`'s reservations) and
[native probing](/quest/m1/probe-native.md), not padding, which is
browser-only. The JS publisher split into its own m2 quest, since it probes
with padding.

- One rule, one home. Put the disable and re-enable decision in
  `moq_mux::rate` beside `Control`, so audio
  ([#2848](/quest/m2/2848-follow-the-bandwidth-grant-in-moq-audio-instead-of.md))
  and the transcode [ladder controller](/quest/m3/ladder/controller.md) reuse
  it. Start from the ladder's band boundary
  (`disable = (max + 2 * lower) / 3`, see the
  [ladder questline](/quest/m3/ladder/README.md)) unless measurement argues
  otherwise. The JS quest mirrors the same rule and constants.
- Catalog state follows what the encoder applied, not what the policy asked.
  Never rewrite the advertised maximum bitrate or remove the rendition.
- Recovery must not depend on demand. Once viewers deselect a disabled rung
  its demand, and so its grant, goes away; evaluate a hypothetical share
  against the current estimate, as the ladder controller plans to.
- Decided with the maintainer on 2026-10-17 (the
  [probe epic](/quest/m2/probe/README.md)): a disabled rung keeps its
  reservation at its ceiling. The session's probe target sums every live
  reservation, so it keeps probing for the stopped rung and the estimate can
  rise to fund it. No padding handle or burst schedule is driven from `Rung`.
- The lowest rendition never disables. Native probing retransmits in-flight
  data, so a session with every rung stopped has nothing to probe with once
  its last packets are acknowledged, and could never re-enable a rung.
- Disable only on a session with a sender. Native probing needs moq-quic's
  BBR3, so a session without it (iroh on noq, or moq-quic configured for
  CUBIC or no congestion control) could never raise its estimate to resume a
  stopped rung; there every rung keeps encoding and follows its grant down,
  as today. Padding stays browser-only rather than covering these. A session
  has a sender when its transport's `poll::Session::probe` returns one, which
  moq-net keeps private until this quest exposes it to the publishers.
- Assign descending track priority down the renditions so the allocator
  fills lower rungs first; the same number breaks ties in send order
  between subscriptions at one subscriber priority.
- OBS: each OBS video encoder is its own rendition
  (`video_tracks` in `cpp/obs/src/moq-output.cpp`), so apply the rule per
  encoder through the generated C++ package, on top of the per-encoder
  grant following (`MoQRate` in `cpp/obs/src/moq-rate.h`).

Tests with mocked time: a shrinking grant disables the top rung first and
keeps the lower ones at their share; a recovering grant re-enables it only
past the hysteresis and after its viewers left; a jittering grant near the
boundary does not flap the catalog; a grant below every boundary keeps the
lowest rung encoding; a session without a sender never disables a rung.
Prove one shaped-uplink run through moq-ffi, including
a path that reopens after the grant fell below the lowest rung.

Public API: the shared rule in `moq_mux::rate` and whatever knobs the
publishers expose for it. Wire: none; `enabled` already exists.

## Required

- [Native probing](/quest/m1/probe-native.md) - the sender that raises the estimate so a disabled rung can resume

## Related

- [JS rung disable](/quest/m2/probe/rung-disable-js.md) - the same rule in `js/publish`, probing with padding
- [Ladder controller](/quest/m3/ladder/controller.md) - the same rung disable for generated transcode ladders
- [OBS multitrack](/quest/m2/obs-multitrack.md) - OBS's native multitrack encoders as the renditions this disables
- [Viewer up-switch](/quest/m2/probe/viewer-upswitch.md) - the viewer side of moving between renditions as capacity changes
- [Closed rung disable](https://github.com/moq-dev/moq/pull/5850) - reference code from before the 2026-10-20 split, closed as superseded
