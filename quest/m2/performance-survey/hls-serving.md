# [L] Measure and reduce repeated HLS serving work

## Goal

HLS/DASH viewer fan-out avoids redundant fetch/remux/render work where
measurements justify sharing, without retaining stale or unauthorized output.

## Plan

[Rendition::fetch](/rs/moq-hls/src/export/rendition.rs) creates a muxer,
fetches group ranges, gathers frames and constructs a fragment on each segment
request (lines 1147-1217). Init construction already has caching; do not count
it as missing. Media playlists snapshot/render on each call, and DASH builds
representations from the same rendition state.

Sweep viewers × renditions with independent window length, frame size and
concurrent same-segment requests. Measure CPU/request, upstream fetch count,
allocations, retained bytes and response tail latency, both cold and warm.
Separate unavoidable response serialization from duplicate content work.
Validate all bytes/durations and include gaps, eviction, restarts, reference
handover, concurrent first requests and authorization differences.

Shared in-flight work or bounded immutable output caching are candidates only
after profiling. Keys and ownership must include the actual content instance;
never let a reused row number cross epochs or cache one viewer's credentials
in another's playlist. The archive/HLS contract quests and
[release audit](https://github.com/moq-dev/moq/pull/6311) retain policy decisions.
This takes ownership of the recurring audit's HLS serving coverage item.

Follow the [survey's benchmark contract](/quest/m2/performance-survey/README.md):
measure the baseline before optimizing, retain paired evidence, verify equivalent
work, schedule coverage nightly, and accept a measured no-win. Expected public
API and wire impact: none; report the actual impact in the implementation PR.
