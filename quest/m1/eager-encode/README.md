# Always-on encoding

## Goal

First-party publishers capture and encode while a source is enabled,
whatever the subscriber demand, so the recent past exists for FETCH and a
subscription's `maxDelay` to serve. On-demand encoding stays available as an
explicit opt-in. Video stays always-on. Audio becomes on-demand again once it
can keep raw frames up to `maxAge` while nobody listens.

## Plan

Decided 2026-10-28 (maintainer), from [#7101](https://github.com/moq-dev/moq/issues/7101):

- **Default always-on, in every language.** A demand-gated encoder leaves
  nothing for a late subscriber to fetch, and FETCH and `maxDelay` make the
  past part of the model. Nobody relies on on-demand encoding; the value is
  in the networking. On-demand becomes an opt-in on each publisher that has
  it today.
- **Rejected for video:** keeping a backbuffer of raw frames, too costly.
  Audio does keep one, in [raw audio on demand](/quest/m1/eager-encode/raw-audio.md),
  where it is cheap.
- **Stay on-demand by default:** moq-transcode rungs (a server-side ladder per
  broadcast, and a past FETCH is already served by transcoding from the
  source), the moq-boy demo, and the CARLA demo. Each has its own gating,
  which is unchanged; their docs present on-demand as their choice.
- Bandwidth reservations stay keyed on demand: nothing is sent without a
  subscriber.
- An unwatched rendition encodes at its configured bitrate (decided
  2026-10-28). It needs no remembered state, a past viewer's grant is stale
  anyway, and the backlog is served from cache rather than paced live.
  Rejected: the last granted target, and the lowest rung.

Already opt-in, with no built-in gate: moq-c (`moq_encode_*_demand`), moq-ffi,
the wrappers, OBS and moq-gst. Only their docs change.

The epic's own work, once the children land:

- Docs: `doc/lib/rs/moq-net.md:25` ("publish only while someone is
  watching"), `doc/concept/use-case/contribution.md` (on-demand becomes an
  option), the moq-c doc comment at `rs/moq-c/src/audio.rs:433`, and the
  Swift/Python/Go/Dart wrapper docs.
- Rewrite the quests that assume demand-gated capture:
  [studio replay](/quest/m3/studio/replay.md) and its compose sources ("arm
  replay = local subscriber"), [iOS capture](/quest/m2/mobile-capture-ios.md),
  [video hardware](/quest/m3/video-hardware.md), and
  [time-addressed consumers](/quest/m1/time-addressing/consumers.md)'s
  HLS-only-viewer clause.

## Required

- [@moq/publish encodes while enabled](/quest/m1/eager-encode/js.md) - audio and video encoders drop the demand gate, with an on-demand opt-in
- [Raw audio on demand](/quest/m1/eager-encode/raw-audio.md) - audio encodes on demand again, from raw frames kept up to `maxAge`

## Related

- [Push-to-talk loses no speech](/quest/m1/push-to-talk/README.md) - the late-subscriber half of an unmute this fixes
