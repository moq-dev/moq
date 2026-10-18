# [L] Publishers disable simulcast rungs the grant cannot fund

## Goal

A publisher encoding several video renditions of one source stops encoding
its top renditions when its bandwidth grant cannot fund them, advertising
each as `enabled: false` in the catalog, and re-enables them with hysteresis
once the grant recovers. The lower renditions keep their share instead of
every rung degrading together. Covers the JS publisher (`js/publish`), the
Rust and FFI publishers (the `rs/moq-video` producer under the `rs/moq-mux`
rate policy), and the OBS plugin. `@moq/watch`, `moq play`, `moqsrc`, WHEP,
HLS and DASH export, and the moq-mux exports already skip disabled renditions
and follow one that flips; moq-ffi and moq-c refuse to subscribe one and leave
the fallback pick to the application.

## Plan

Today each video encoder reserves its configured ceiling and follows its own
grant down (`js/publish/src/video/encoder.ts`,
`rs/moq-video/src/encode/producer.rs`), so a congested uplink squeezes every
rendition at once and a top rung starved of most of its share keeps
encoding. JS also takes a manual `enabled` input that already publishes
`enabled: false` and stops encoding; this quest drives that state from the
grant.

- One rule, one home. Put the disable and re-enable decision in
  `moq_mux::rate` beside `Control`, so audio
  ([#2848](/quest/m2/2848-follow-the-bandwidth-grant-in-moq-audio-instead-of.md))
  and the transcode [ladder controller](/quest/m3/ladder/controller.md) reuse
  it. Start from the ladder's band boundary
  (`disable = (max + 2 * lower) / 3`, see the
  [ladder questline](/quest/m3/ladder/README.md)) unless measurement argues
  otherwise. JS mirrors the same rule and constants.
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
- Assign descending track priority down the renditions so the allocator
  fills lower rungs first; [Scope track priority](/quest/m1/track-priority-scope.md)
  settles how that number also orders sends.
- OBS: each OBS video encoder is its own rendition
  (`video_tracks` in `cpp/obs/src/moq-output.cpp`), so apply the rule per
  encoder through the generated C++ package, on top of the per-encoder
  grant following (`MoQRate` in `cpp/obs/src/moq-rate.h`).

Tests with mocked time: a shrinking grant disables the top rung first and
keeps the lower ones at their share; a recovering grant re-enables it only
past the hysteresis and after its viewers left; a jittering grant near the
boundary does not flap the catalog. Prove one shaped-uplink run in the
browser and one through moq-ffi.

Public API: the shared rule in `moq_mux::rate` and whatever knobs the
publishers expose for it. Wire: none; `enabled` already exists.

## Required

- [Session probe target](/quest/m2/probe/target.md) - a disabled rung's reservation keeps the session probing for it, since the estimators stop learning while app-limited
- [Padding probes](/quest/m2/probe/padding.md) - the sender a browser publisher probes with, and any transport without native probing

## Related

- [Ladder controller](/quest/m3/ladder/controller.md) - the same rung disable for generated transcode ladders
- [OBS multitrack](/quest/m2/obs-multitrack.md) - OBS's native multitrack encoders as the renditions this disables
- [Viewer up-switch](/quest/m2/probe/viewer-upswitch.md) - the viewer side of moving between renditions as capacity changes
