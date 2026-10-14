# [M] HLS and DASH follow an enabled reference rendition

## Goal

When the rendition an HLS or DASH edge takes its segment boundaries from is
disabled, every enabled sibling's playlist and MPD keep advancing. The edge
hands the reference to an enabled rendition, and each playlist's media
sequence stays continuous across the handoff.

## Plan

Found in review of [#5471](https://github.com/moq-dev/moq/pull/5471)
(2026-10-13): every playlist takes its segment boundaries from one reference
rendition, and `reference()` ignores `enabled`. The top rung sorts first, and
it is the first rung [Simulcast rung disable](/quest/m2/simulcast-rung-disable.md)
turns off, so disabling it freezes every sibling. This predates #5471. The
hang draft says an edge keeps its video reference while it stays in the
catalog.

Decided 2026-10-13 (maintainer): a separate quest, Required by simulcast
rung disable; #5471 merged without it.

- Pick the reference among enabled renditions, and hand it over when the
  current one is disabled, at a segment boundary both renditions share.
- Keep each media sequence and segment numbering continuous, so a player
  sees no discontinuity unless the content has one.
- Amend `drafts/draft-lcurley-moq-hang.md` and `doc/bin/hls.md` where they
  say the edge keeps its reference while it stays in the catalog.
- Regression with mocked time: disable the reference rung mid-stream, and
  every enabled sibling's playlist keeps advancing with a continuous
  sequence.

Public API: none expected. Format: the hang draft's reference rule changes.
