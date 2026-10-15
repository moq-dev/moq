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

Decided 2026-10-15 (maintainer): preserve source-addressed segment URLs,
use a separate continuous HLS media sequence, and start a new DASH Period
on reference changes. If no shared boundary remains at the advertised
tail, switch at the next replacement boundary with a gap/discontinuity;
this exception must not change an already-advertised source span.

- Pick an enabled reference and hand it over at a shared boundary when
  possible, otherwise at the next replacement boundary with a discontinuity.
- Keep each HLS media sequence continuous, while URI segment numbers keep
  their source reference and sequence. Keep discontinuity sequences stable
  when older segments retire.
- Amend `drafts/draft-lcurley-moq-hang.md` and `doc/bin/hls.md` where they
  say the edge keeps its reference while it stays in the catalog.
- Regression with mocked time: disable the reference rung mid-stream, and
  every enabled sibling's playlist keeps advancing with a continuous
  sequence. Cover aligned and unaligned tails, retained source URLs, recorder
  cursors, and durable-history retention.

Public API: none expected. Format: the hang draft's reference rule changes.
