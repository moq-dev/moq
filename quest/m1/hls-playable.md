# [M] Playable

## Goal

Long-lived broadcasts cannot become permanently unplayable. A 24/7 broadcast is
the normal CDN case, so the origin must not degrade with uptime.

## Plan

The mechanism that remains is the timeline's shape, not the 500s originally
reported downstream. A media timeline is a `moq_json::stream` on one
never-rolled group (`rs/moq-mux/src/timeline.rs`), so a long-lived broadcast's
index grows without bound, held by the publisher and read from the start by
every new origin. The `dev` branch keeps that single group after
[moq#2547](https://github.com/moq-dev/moq/pull/2547). That rework makes it one
timeline per broadcast rather than one per rendition, which lowers the rate but
not the shape. An aged-out group already maps to 404 rather than 500
([moq#2615](https://github.com/moq-dev/moq/pull/2615)), so re-verify the
reported symptom before chasing it.

The fix is the archive questline's [archive timeline](/quest/m1/archive/timeline.md)
rather than a second mechanism here: the merged `moq_json::Window` bounds and
rolls the timeline, so a joiner reads one current group instead of history since
startup. The exporter's playlist window then derives from that shared Window.

Prove it with a long-running broadcast rather than a unit test: a fresh viewer
joining a broadcast that has been up for days must get a playlist promptly.

## Required

- [Archive timeline](/quest/m1/archive/timeline.md) - the timeline must adopt the merged Window before a long-lived broadcast can stay playable
