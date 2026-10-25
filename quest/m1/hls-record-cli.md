# [M] HLS recorder in the CLI

## Goal

`moq ... export hls --output <dir>` writes the broadcast's playlists and
segments to a directory as they complete, using moq-hls's recorder cursors,
and exits with an error if the recording ends abruptly or misses a segment.
It works alone or alongside `--listen`, so one stage can serve and record at
once.

## Plan

Decided 2026-10-23 (maintainer interview):

- A flag beside `--listen`, not a positional target or an object-store URL.
- Recording alone binds no port: `--listen` defaults to `[::]:8089` today, so
  that default must not apply when `--output` is given without `--listen`.
- This is the cursors' first consumer outside moq-hls's tests.
- Update `doc/bin/cli.md` and `doc/bin/hls.md`, and every example that uses
  `export hls`.

Decided 2026-10-24 (maintainer interview): a full recording stays playable from
its master playlist after decoder reconfiguration. Each rendition keeps one
continuous media playlist; separate instance directories would leave the master
listing only current instances, losing the recording's earlier part.

- Reconfiguration closes the old rendition and adds a replacement over the same
  shared rows. The replacement's cursor starts at its oldest retained row, so it
  repeats segments the old cursor wrote. Drain the old cursor first: it owns every
  segment it wrote. Discard replacement segments numbered at or below the old
  cursor's last, then write the replacement's init and mark the transition.
- Discarding by number is sound: every rendition's rows fan out from one reference
  timeline, so a number names the same content span on each, and numbers only
  increase within a broadcaster run, including across reference handoffs.
- A rewound timeline or a new generation can reuse lower numbers, but it also
  advances `Segment::discontinuity`. A repeated number with a newer discontinuity
  is a source reset and fails the recording instead of being discarded.
- A true gap must fail too. The cursor skips declared gaps silently, so a number
  jump between the two cursors cannot tell a declared gap from rows evicted before
  the replacement was subscribed. Make that boundary observable, such as by having
  the cursor report gap rows, which the recording needs for `EXT-X-GAP` anyway.
- Mark the transition at the same segment in every playlist of the recording, so
  their discontinuity sequences stay matched.
