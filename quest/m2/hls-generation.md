# [L] HLS generation

## Goal

Media URLs carry a generation ID so segment caching can be re-enabled safely. The
same ID keeps a restarted publisher from being decoded against the previous run's
init segment.

## Plan

Two bugs, one missing ingredient. A CDN edge must serve `.../seg/{n}.m4s` and
`init.mp4` with `no-store` today because the URLs are reused across publisher
generations. And `Rendition::init` caches the init for the life of the pooled
rendition with no invalidation, so an inline-parameter-set codec (`avc3`) that
restarts at a new resolution decodes new segments against the old parameter
sets; the segment window's `push` (`rs/moq-hls/src/export/segments.rs`) detects
that restart already and resets only the playlist.

The identity itself is [Broadcast epoch](/quest/m2/broadcast-epoch.md)'s job, and it arrives from the
TRANSPORT rather than from the media: `TRACK_INFO` returns the resolved epoch.
Never substitute wall-clock or a relay-local counter, since a CDN needs one URL to
mean the same bytes on every edge.

So this quest is the consumption: the moq-hls playlist and MPD renderers EMIT
epoch-versioned `init.mp4` and segment URLs, and the cached init is invalidated
when the epoch changes. Both are CONDITIONAL on a non-zero resolved epoch: a
publisher that mints none resolves to 0, which cannot version anything, so
those broadcasts keep unversioned URLs rather than being cached against an
identity that does not exist. The cache-header half is moq.pro (downstream)
work: its edge parses the versioned paths and restores the long `max-age`
(where `immutable` is finally true) once the renderers emit them; versioning
the accepted paths without versioning the emitted ones changes nothing.

**Open: the broadcast epoch does not cover a mid-broadcast rendition
reconfigure.** Only one of the two collision sources is a new generation: an
epoch is minted per path when the broadcast is announced, so a live publisher
that reconfigures a rendition (a resolution change, say) reuses the same
`init.mp4` URL and segment numbers for different bytes without the epoch moving.
Re-enabling caching on the epoch alone would let a CDN keep serving the previous
configuration. Resolve before caching is re-enabled, either by adding a
rendition-generation component to the URL or by requiring (and testing) an epoch
bump on every reconfigure.

Acceptance: a publisher restart mid-playback exercised end to end, a zero-epoch
publisher still served with unversioned URLs, and a mid-broadcast reconfigure
that cannot be decoded against the previous configuration.

## Required

- [Broadcast epoch](/quest/m2/broadcast-epoch.md) - a generation ID has to exist before a URL can carry it
