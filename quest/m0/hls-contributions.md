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

**Reading the catalog is the listing (decided).** A master playlist variant
needs a `BANDWIDTH` and codecs, so moq-hls cannot list a contribution's
renditions without its catalog. The maintainer decided on 2026-11-02 that the
master reads each contribution's catalog when it is served: only the
contribution's media waits for a player's media playlist request, and a
catalog read starts no encoding. The master opens each contribution's catalog
while the broadcast is being served and follows it, but no media track. The
hang draft's Contributions section says a consumer opens a contribution's
catalog only when it selects it; an HLS master selects every contribution it
lists, so add that to the draft, and that a publisher producing media on
demand answers the catalog without starting media. `moq-transcode` already
does: only rung track demand starts an encode.

**Follow each contribution's instances.** A contribution is its own broadcast
with its own catalog, so follow it the way the server follows a broadcast it
serves, not the way a rendition binds a sibling. `Source::bind` pins a path's
first answer and never resolves it again after a replacement
(`rs/moq-mux/src/source.rs`), and a rendition whose bound sibling ends ends
with it (`Media` in `rs/moq-hls/src/export/rendition.rs`). So resolve each
contribution entry with `Source::follow` (`moq_mux::follow::Instances`),
through the server's own origin, and build a `moq_mux::Source` for each
instance it hands out, so the instance's catalog and media reads stay pinned
to that instance. A new instance replaces the contribution's renditions and an
ended one drops them from the master until the next one starts. An entry
naming an `instance` follows only that stream, as the draft's
`instance` rules require. Unresolved, a contribution is listed once it
resolves; denied by the origin's scope, it is left out of the master and its
URLs answer `403`. Do not add a second resolver.

**Segments line up with the broadcast's.** Players switch variants by
segment number, so a contribution's renditions share the broadcast's grid:
its catalog must declare the same `media.join.interval` on the same timeline.
A contribution declaring another interval or none is not HLS-renderable with
the broadcast and is left out of the master, as a rendition that cannot be
exported is today.

**Lazy media.** A contribution rendition holds no media subscription until a
request reaches its URLs. The first one subscribes to that rendition's tracks
only, and its media playlist answers once it lists a segment. The
subscription ends once no request to the rendition is in progress and none has
finished for longer than a player reloading at the target duration would allow
(RFC 8216 6.3.4); a slow response, such as a first media playlist waiting on an
on-demand encode, keeps it. Pick the multiple and say why in the code, and a
later request starts it again.
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
starts demand on that rendition's tracks only, a request still in progress
past the idle window keeps it, idle release ends it, and a later request starts
it again. A contribution that is unannounced at first is
listed once it resolves. A restarted contribution is followed onto its new instance, and one naming an
`instance` is not. A denied one is
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
