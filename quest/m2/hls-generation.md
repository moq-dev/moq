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

The missing ingredient is the identity itself: nothing in the stack mints a
generation today. Where it comes from is
[Plan: HLS identity](/quest/m2/plan-hls-identity.md)'s job. Whatever it turns
out to be, it must be minted by the publisher and travel with the content;
never a wall-clock or a relay-local counter, since a CDN needs one URL to mean
the same bytes on every edge.

So this quest is the consumption: the moq-hls playlist and MPD renderers EMIT
generation-versioned `init.mp4` and segment URLs, and the cached init is
invalidated when the generation changes. Both are CONDITIONAL on the identity
being present: a publisher that declares none cannot version anything, so those
broadcasts keep unversioned URLs rather than being cached against an identity
that does not exist. The cache-header half is moq.pro (downstream) work: its
edge parses the versioned paths and restores the long `max-age` (where
`immutable` is finally true) once the renderers emit them; versioning the
accepted paths without versioning the emitted ones changes nothing.

There are two collision sources, and a per-broadcast identity only covers one
of them. A live publisher that reconfigures a rendition (a resolution change,
say) reuses the same `init.mp4` URL and segment numbers for different bytes
without restarting the broadcast, so caching on a restart-scoped generation
alone would let a CDN keep serving the previous configuration. Either the URL
gains a rendition-generation component or the identity is required (and tested)
to move on every reconfigure. Settle it in
[Plan: HLS identity](/quest/m2/plan-hls-identity.md); it is the reason that
plan exists rather than a detail of this one.

Acceptance: a publisher restart mid-playback exercised end to end, a publisher
declaring no identity still served with unversioned URLs, and a mid-broadcast
reconfigure that cannot be decoded against the previous configuration.

## Required

- [Plan: HLS identity](/quest/m2/plan-hls-identity.md) - a generation ID has to exist before a URL can carry it
