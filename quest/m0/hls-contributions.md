# [L] HLS lists a broadcast's contributions without starting their media

## Goal

A moq-hls master playlist lists the audio and video renditions of each
contribution in the broadcast's catalog (the root `contributions` map) beside
the broadcast's own. It reads a contribution's catalog to list them, and
subscribes to a contribution rendition's media tracks only once a player
requests that rendition, so an on-demand contribution, such as a transcode
ladder, encodes only the rungs players fetch. Once no player fetches a
contribution rendition, its media subscription ends.

## Plan

**Reading the catalog is the listing.** A master playlist variant needs a
`BANDWIDTH` and a media playlist, so moq-hls cannot list a contribution's
renditions without its catalog. The master opens each contribution's catalog
while the broadcast is being served and follows it, but no media track. The
hang draft's Contributions section says a consumer opens a contribution's
catalog only when it selects it; an HLS master selects every contribution it
lists, so add that to the draft, and that a publisher producing media on
demand answers the catalog without starting media. `moq-transcode` already
does: only rung track demand starts an encode.

**Resolve like a rendition's reference.** Bind each contribution with
`moq_mux::Source`'s binding and pinning (`Source::bind`, as
`rs/moq-hls/src/export/rendition.rs` binds a rendition's `broadcast`
reference), through the server's own origin. A contribution follows the same
rules as a sibling reference today: denied by the origin's scope, it is left
out of the master and its URLs answer `403`; unresolved, it is listed once it
resolves; restarted, its renditions follow the new instance. Do not add a
second resolver.

**Segments line up with the broadcast's.** Players switch variants by
segment number, so a contribution's renditions share the broadcast's grid:
its catalog must declare the same `media.join.interval` on the same timeline.
A contribution declaring another interval or none is not HLS-renderable with
the broadcast and is left out of the master, as a rendition that cannot be
exported is today.

**Lazy media.** A contribution rendition holds no media subscription until a
request reaches its URLs. The first one subscribes to that rendition's tracks
only, and its media playlist answers once it lists a segment. The
subscription ends once no request has reached the rendition for longer than a
player reloading at the target duration would allow (RFC 8216 6.3.4); pick the
multiple and say why in the code, and a later request starts it again.
Today's renditions keep their eager subscription: the broadcast's own media is
published anyway, so only contributions change.

**URLs and names.** A contribution's rendition IDs belong to its own catalog,
so its URLs nest under the contribution ID inside the broadcast's instance
(for example `@{instance}/contribution/{id}/{kind}/{rendition}/media.m3u8`;
the implementation settles the spelling), and audio `NAME`s stay unique within
their group. A path-based gate in front of the server sees the broadcast's
path, so a viewer allowed the broadcast can play its contributions, as with a
sibling reference. Video variants join the broadcast's audio groups.

**Out of scope, with reasons.** The DASH manifest keeps listing only the
broadcast's own renditions: a `$Time$` timeline needs every listed
representation's media, which defeats the lazy start. The recorder's
`renditions::Consumer` keeps yielding only the broadcast's own renditions: a
contribution is its own broadcast with its own recording.

**Tests.** A master lists the broadcast's and a contribution's renditions
while the contribution's media tracks have no demand. A media playlist request
starts demand on that rendition's tracks only, idle release ends it, and a
later request starts it again. A contribution that is unannounced at first is
listed once it resolves. A restarted contribution is followed. A denied one is
left out and its URLs answer `403`. A mismatched or missing interval leaves the
contribution out. A contribution rendition sharing a name with one of the
broadcast's own gets distinct URLs and names. Unit tests mock time for the idle
release.

Update `doc/bin/hls.md` and the hang draft's Contributions section.

Public API: additive in moq-hls (the new URL routes). Wire: none; the draft
gains the HLS listing rule and the on-demand publisher's catalog answer.

## Required

- [HLS export on a declared grid](/quest/m0/hls-media-grid.md) - the export reads media tracks on `media.join.interval`, which both the shared segment numbering and the lazy media subscription build on

## Related

- [The HLS server follows each path once](/quest/m2/hls-follow-once.md) - also changes how the server resolves a path
