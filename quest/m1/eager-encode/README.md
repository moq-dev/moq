# [S] Always-on encoding

## Goal

First-party publishers capture and encode while a source is enabled,
whatever the subscriber demand, so the recent past exists for FETCH and a
subscription's `maxDelay` to serve. On-demand encoding stays available as an
explicit opt-in. Video stays always-on. Audio encodes on demand, from raw
frames kept up to `maxAge` while nobody listens.

What remains: `moq-studio`'s program video still copies and encodes only while
someone watches (`watch` and `run_program` in `rs/moq-studio/src/engine.rs`,
the Output panel's "none; not encoding", and `doc/bin/studio.md`'s "Video is
only encoded while someone watches it"). No child covered it, and the
library, CLI, browser, wrapper, and room publishers are done.

## Plan

Decided 2026-10-28 (maintainer), from [#7101](https://github.com/moq-dev/moq/issues/7101):

- **Default always-on, in every language.** A demand-gated encoder leaves
  nothing for a late subscriber to fetch, and FETCH and `maxDelay` make the
  past part of the model. Nobody relies on on-demand encoding; the value is
  in the networking. On-demand becomes an opt-in on each publisher that has
  it today.
- **Rejected for video:** keeping a backbuffer of raw frames, too costly.
  Audio does keep one, where it is cheap
  ([moq-dev/moq#7238](https://github.com/moq-dev/moq/pull/7238)).
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

The studio quests already assume the program and its sources encode whatever
the demand ([replay](/quest/m3/studio/replay.md),
[source tracks](/quest/m3/studio/compose/sources.md)). Whether the program
drops its gate here, with which opt-in, or the studio is exempted like
moq-transcode is open on this quest's PR.

## Related

- [Push-to-talk loses no speech](/quest/m1/push-to-talk/README.md) - the late-subscriber half of an unmute this fixes
