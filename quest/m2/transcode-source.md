# [M] Choose a locally decodable transcode source

## Goal

A higher-resolution rendition that this host cannot decode does not prevent
transcoding another usable rendition in the same source catalog.

## Plan

catalog::choose_source filters by codec syntax and dimensions, then ranks the
largest rendition. It does not establish whether the selected backend can
decode it. On a software-only host, a larger H.265 or AV1 entry can win over a
usable H.264 entry and fail later when demand opens the decoder.

Reproduce that case with deterministic backend capabilities and preserve the
explicit backend selection policy. Choose among locally supported candidates
or return an actionable refusal when none exists. Avoid repeated encoder or
decoder probes for every catalog edit, and do not turn malformed stream input
into silent fallback after a rendition is already being consumed.

Test mixed-codec catalogs, an explicitly forced unavailable backend, no usable
candidate, and a catalog update while the selected rendition remains valid.
Keep rendition identity, output names, and existing live/fetch behavior stable.
Run the regressions in CI. Public API and wire: unchanged.

## Required

- [Video output](/quest/m0/video-output.md) - use the settled decoder configuration
