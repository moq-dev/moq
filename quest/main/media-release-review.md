# [S] Verify the four media contracts before 0.1

## Goal

Record that the agreed audio, video, transcode, and NVENC API changes have
landed, or return concrete remaining blockers, before a separate version-bump
request. Completing this review does not bump or publish packages.

## Plan

Review the final public surface and examples after the prerequisite quests.
Check that deferred backend, surround, refresh, color, and performance work can
use the extension points without another planned replacement of these APIs.
Search the living backlog for obsolete signatures and remove completed work.

Verify independent crate feature builds, default and backend-free refusal,
ownership/cancellation tests, docs, and migrated callers on the exact release
candidate. Use the Nix check/test recipes and platform CI; record skipped
hardware cases separately from compilation. Confirm package manifests do not
reintroduce mandatory native compilation through feature unification.

No published FFI/C layout or wire break is authorized by this stabilization
line. Any newly discovered break outside these four 0.0.x crates needs its own
maintainer decision and appropriate branch. No performance claim is accepted
without measurements; unresolved optimization work alone does not block 0.1.

Public API/wire impact: review only.

## Required

- [NVENC resources](/quest/main/nvenc-resources.md) - safe ownership is enforced
- [NVENC loading](/quest/main/nvenc-loading.md) - driver refusal is fallible
- [Codec threads](/quest/main/video-thread-ownership.md) - direct handles have correct auto traits
- [Media features](/quest/main/media-features.md) - independent build choices work
- [Shared rate policy](/quest/main/media-rate-policy.md) - later adaptation will not remove a stabilized namespace
- [Audio configuration](/quest/main/audio-config.md) - PCM and codec contracts are settled
- [Audio publication](/quest/main/audio-publication.md) - only intended authority is public
- [Remove ineffective FEC](/quest/main/audio-fec.md) - no unsupported promise remains
- [AEC ownership](/quest/main/audio-aec.md) - microphone attachment is exclusive
- [Playback outcome](/quest/main/audio-playback.md) - drops are observable
- [Video output](/quest/main/video-output.md) - output and subscription contracts are distinct
- [Video frames](/quest/main/video-frames.md) - pixels and extension points are typed
- [Video timing](/quest/main/video-timing.md) - timestamps and rates retain their meaning
- [Video GOP](/quest/main/video-gop.md) - deferred refresh does not replace the group API
