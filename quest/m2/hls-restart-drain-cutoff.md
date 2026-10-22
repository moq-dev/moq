# [M] Finite drain for a replaced run's HLS recording

## Goal

When a broadcast's publisher announces `Restart`, a moq-hls consumer that is
still recording the old run can finish that recording at a finite, defined
boundary, even if the old publisher keeps sending media. The successor run
starts at once and never waits on the old drain. The old recording keeps every
completed segment it accepted before the boundary and nothing after it.

## Plan

Today an open subscription stays on the old run until its route goes, so an
old publisher that keeps sending can keep its recording open indefinitely.
Catalog EOF and announce `End` already finalize a recording cleanly
([moq#6087](https://github.com/moq-dev/moq/pull/6087)); a replaced run has no
equivalent cutoff.

Open question for the maintainer: what defines the boundary. Candidates:

- A timeline-head cutoff: the old run's recording ends at the media time the
  successor's first group starts.
- A group cutoff: accept groups already open at `Restart`, refuse new ones.
- A bounded grace window in media time (not wall time).

Rejected: an unbounded drain of the old run; an immediate cutoff at `Restart`
that discards completed backlog; treating a catalog error as media EOF.

Tests: an old publisher that keeps sending after `Restart` still finalizes
with exactly its accepted tail; the successor's recording starts before the
old one finishes; catalog EOF and catalog-error behavior are unchanged.
Expose the boundary through the moq-hls recording API; no wire change is
expected, so report any API impact.
