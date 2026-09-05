# [M] HLS generation

## Goal

`moq-hls` emits media URLs a CDN can cache. The init segment URL changes
whenever the rendition it initializes changes, and an embedder can hand the
exporter a generation string that every segment URL carries, so a restarted
publisher is never decoded against the previous run's init or served under
the previous run's segment numbers. A publisher that provides no generation
keeps today's unversioned URLs.

## Plan

Two collision sources, settled separately after a 2026-09 planning pass.

**A live reconfigure** reuses `init.mp4` for different bytes: an
inline-parameter-set codec (`avc3`) that restarts at a new resolution keeps
its URL while `Rendition::init` caches the old init for the life of the
pooled rendition, and the segment window's `push`
(`rs/moq-hls/src/export/segments.rs`) resets only the playlist. Version the
init URL with a hash of the rendition config (codec string, dimensions, and
the parameter sets the init carries), so a reconfigure changes the URL and
the cached init is invalidated with it. A hash rather than a counter because
two edges rendering the same broadcast must agree without coordinating.

**A publisher restart** reuses segment numbers: `moq-net` splices a
re-attached source into the existing broadcast by hop ID so consumers never
observe it, and group sequences restart at zero. No protocol carries an
identity for this and none will: an epoch on the announce messages was
rejected (announcements are capability routes that must keep stitching by hop
ID), and a `TRACK_INFO` epoch, a catalog `generation`, and an epoch path
segment were all declined in favor of the managed edge minting one per
publisher session downstream. So `moq-hls` gains an optional generation input,
one string per broadcast the embedder supplies, and when it is present the
playlist and MPD renderers emit it in `init.mp4` and segment URLs and the
export resets its window when it changes. Absent means unversioned URLs, never
a wall-clock or a relay-local counter. The cache-header half stays downstream:
the edge restores its long `max-age` once the paths it accepts are the ones the
renderers emit.

Acceptance: a mid-broadcast reconfigure that cannot be decoded against the
previous init gets a new init URL on both edges; a supplied generation appears
in every media URL and a changed one resets the export; no generation keeps
the URLs byte-identical to today.
