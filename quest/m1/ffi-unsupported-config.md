# [S] moq-ffi reports an unsupported media configuration as a typed error

## Goal

`MoqAudioConsumer::subscribe` and the matching video subscribe fail with a
typed "unsupported configuration" error (a distinct `MoqError` variant, not a
reason string) when moq-audio or moq-video refuses a rendition's codec,
layout, or description. The OBS source falls through to the next ranked
rendition on that error and drops its copy of moq-audio's codec checks.

## Plan

Decided 2026-10-31 (user, /quest-plan of the follow-ups from the 2026-10-31
/quest-complete run), from
[moq-dev/moq#7331](https://github.com/moq-dev/moq/pull/7331).

Milestone: ✅ m1 (moved from m2). Not chosen: m2.

- [moq-dev/moq#7331](https://github.com/moq-dev/moq/pull/7331) gates audio in OBS with `moq_source_audio_decodes` (opus, pcm, mono
  or stereo AAC-LC), mirroring moq-audio's rules, because moq-ffi reports a
  setup refusal only as an async `MoqError::Audio { reason }`
  (`rs/moq-ffi/src/error.rs:235`; video at `:243`). A configuration moq-audio
  refuses under a supported codec string, such as an AAC description that
  declares SBR, still discards the candidate without trying the next.
- Add the typed variant, raised from `subscribe` (`rs/moq-ffi/src/audio.rs:656`,
  `video.rs:1296`) wherever moq-audio or moq-video refuses the config before
  decoding starts. Mirror it in moq-c and the wrappers.
- In OBS, fall through on the typed error and delete the mirrored gate and
  its tests.
- Not in scope: falling through after a later async failure (subscribe
  rejected by the peer, first decode fails), which [moq-dev/moq#7331](https://github.com/moq-dev/moq/pull/7331) also listed.

Public API: adds a `MoqError` variant (bindings and moq-c). Wire: none.

## Required

- [OBS source picks renditions by the catalog contract](/quest/m2/obs-source-renditions.md) - [moq-dev/moq#7331](https://github.com/moq-dev/moq/pull/7331) adds the ranked fall-through this hooks into
