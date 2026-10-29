# [M] JS consumers hold a disabled rendition

## Goal

`@moq/watch` keeps its selected audio or video rendition, and its
subscription, while the publisher has it `enabled: false` and no enabled
alternative exists. Re-enabling it plays without a resubscribe. The hang spec
says a consumer "should not select" a disabled rendition, where it said "must
not".

## Plan

Decided 2026-10-28 (maintainer), default on, audio and video:

- Initial selection still skips disabled renditions, so a listener who joins
  during a pause subscribes on the unmute.
- The selected rendition stays selected while it is disabled, still in the
  catalog, and every rendition of its kind is disabled. An enabled
  alternative wins, so a simulcast ladder (whose lowest rung never disables)
  never holds. A rendition removed from the catalog is dropped.
- Audio: `#runSelected` (`js/watch/src/audio/source.ts:129-133`) and the
  decoder's subscription (`audio/decoder.ts:363-396`). Key `#idle`
  (`decoder.ts:203-205`) on "held and disabled" as well as a missing config,
  so a muted tile still suspends its AudioContext.
- Video: `enabledRenditions` (`js/watch/src/video/source.ts:190-191`) with the
  same rule.
- A held subscription gets a clean break from the publisher's discontinuity
  marker ([#4880](https://github.com/moq-dev/moq/pull/4880)), or after
  [time addressing](/quest/m1/time-addressing/consumers.md) from the gap
  between groups, and keeps its
  audio graph ([#4881](https://github.com/moq-dev/moq/pull/4881)).

Spec wording lands with whichever hold quest lands first: the hang draft,
`rs/hang/src/catalog/{audio,video}/mod.rs`, `js/hang/src/catalog/audio.ts` and
`video.ts`, and `doc/concept/hang.md`.
[OBS source renditions](/quest/m2/obs-source-renditions.md) already plans the
same hold.

Tests: mute, then unmute, keeps one subscription. An enabled alternative takes
over. A rendition removed while held is dropped. A held muted tile suspends
its AudioContext.

Public API: behavior only. Wire: none.

## Related

- [Rust consumers hold a disabled rendition](/quest/m1/push-to-talk/hold-rs.md) - the same rule in Rust
