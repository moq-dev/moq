# [M] moq-transcode: run() can't bootstrap from a demand-gated source that doesn't advertise its geometry

## Goal

Implement and verify the behavior tracked in [#2788](https://github.com/moq-dev/moq/issues/2788)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

`moq_transcode::run` can't bootstrap from a publisher that only encodes while watched, unless that publisher advertises its geometry before encoding anything.

The cycle:

1. `run` waits for a catalog snapshot with a usable video rendition, and only subscribes to the source video track afterwards (`feed::Feed::new` is constructed after the `choose_source` loop, and even then the subscribe happens lazily in `Feed::listen`, when a rung is actually demanded).
2. `resolve_rungs` needs `coded_width`/`coded_height` to size the ladder.
3. A demand-gated publisher (`moq_video::encode::publish_capture`, moq-boy) doesn't open its encoder until something subscribes to its video track.

So when the transcoder is the intended consumer, nobody subscribes, no keyframe is produced, the geometry never arrives, and `run` waits forever. This is the same shape as #2757: a consumer that needs the catalog first, and a publisher that won't produce until demanded.

\#2768 fixes the case where the publisher's geometry *is* known up front (`moq capture --width 1920 --height 1080` now advertises it before the camera opens, so the ladder resolves, a viewer subscribes to a rung, and only then does the camera open). The case that remains stuck is a publisher that advertises its codec but not its picture, which is what capture does when you don't pin a size, because a camera's negotiated mode isn't knowable without opening it.

Before #2768 this scenario hung too (capture published no video rendition at all until its first keyframe), so this isn't a regression from that PR. It's the part of the deadlock that publishing the rendition earlier doesn't reach.

Fix direction: give `run` a bootstrap path that can create demand on the source before it has a ladder, so the source is allowed to produce the keyframe that reveals its geometry. Subscribing to the source video track up front (and dropping it if no rung is ever wanted) would do it, at the cost of opening the camera to learn its mode. Alternatively the ladder could resolve lazily, per rung, once the feed yields its first decoded frame.

Found by Codex while reviewing #2768.

## Closes

- [#2788](https://github.com/moq-dev/moq/issues/2788) - close this issue when the quest finishes
