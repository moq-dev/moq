# [L] One continuous broadcast clock at the catalog root

## Goal

The published catalog and clock API express one continuous broadcast clock:
wall time is a fixed epoch plus PTS converted from the track's timescale.
Existing archive readers and writers use this mapping before release; native
and browser publisher adoption is a separate M2 quest. Wall time is metadata
for applications, not a library playback synchronization source.

## Plan

Reuse the broadcast's existing `moq_mux::Clock`, shared through the catalog
producer. It advances from one monotonic Instant. Establish its corresponding
wall epoch once; sampling wall time when a delayed first frame arrives must
account for that frame's PTS rather than pretend it is timestamp zero.

The current Hang timeline is a broadcast-wide segment index advertised under
Archive, and `timeline::Config.wall` already describes its fixed mapping.
There are no per-rendition timeline producers or `set_wall` methods to wire.
Provide the functional shared clock owner and explicit media-timescale
conversion here. Native capture, CLI imports, and `js/publish` adoption belong
to the publisher integration quest; GStreamer has its own adapter quest.

The clock contract supports translating a reset source onto its existing
mapping, including real idle gaps. Exercise that operation with synthetic
sources here; each publisher adapter wires its restart detection in M2. Preserve permitted B-frame
reordering within a group. A discontinuity marker signals a delivery/playhead
event, not a new wall epoch. Do not overwrite wall, introduce per-record
anchors, or retime retained records after a system-clock adjustment. Refuse a
source that cannot be mapped consistently within that broadcast.

Advertise the fixed broadcast clock at the Hang catalog root, independently
of Archive. A live-only publisher must not create an archive timeline just to
expose its clock. Every media track and any archive index refer to that one
mapping after timescale conversion; there must not be competing wall epochs.
The root shape is `clock: { wall, timescale }`: `wall` is PTS zero in clock
timescale units since the existing MoQ epoch, 2020-01-01T00:00:00Z. Each media
track and the archive index keep their own timescales; conversion into this
one clock is explicit. Validate nonzero timescales and JSON-safe integer
bounds rather than truncating.

Replace the old Archive.timeline.wall field on dev. Update Rust/JS catalog
models, publishers, readers, HLS wall-time mapping, fixtures, documentation,
and the Hang draft in the same PR. Do not retain two independently editable
wall fields or ship a compatibility alias. Existing `timeline::Config.wall`
plumbing must migrate to the same broadcast clock owner. Report the published
API break and wire/catalog migration in the PR; do not bump package versions.
Use a packaged catalog-reader fixture to verify the new shape and wire the
clock tests into CI.

Clock-owner and catalog fixtures cover delayed first timestamps, multiple
track timescales, reset translation with an idle gap, preserved archive records,
numeric bounds, and wall-clock adjustment without retiming. Test the existing
archive/HLS migration end to end. Full capture and publisher restart fixtures
belong to the M2 integration; do not claim those adapters are already wired.

## Related

- [Publisher clocks](/quest/m2/publisher-clock.md) - adopts the clock contract across native capture, CLI import, and browser publication

- [GStreamer wall clock](/quest/m2/3021-moq-gst-anchor-generated-media-timelines-to-wall-clock.md) - selects one trusted clock mapping for every pad
- [Browser wall access](/quest/m2/2278-watch-absolute-wall-clock-latency-target-for-synchronized.md) - exposes the same broadcast clock to applications
- [Cross-track correlation](/quest/m3/teleop/correlation.md) - applications compare clocks they know are synchronized
