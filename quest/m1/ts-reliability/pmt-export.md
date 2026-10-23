# [M] Synced TS legs revise their PMT identically

## Goal

Two `moq export ts --sync` legs of a broadcast whose layout changes
mid-stream revise their PMT identically: the same `version_number`, in the
same slot, listing the same streams, wherever both legs hold the content
around it. A track added no longer ends a synced export.

## Plan

Outside `--sync`, the export follows a PMT revision at the next
`version_number`: a stream joins the PMT with its first frame, and leaves
once the catalog has dropped it and nothing of it is left to go out. A track
recorded on the PID of a dropped stream waits for that stream to drain, and
cuts it if it never ends. Each revision is one boundary: versions go out in
order, no packet of a stream a revision adds goes ahead of it, and its PCR
PID and `muxRate` take over after its last packet. The PCR keeps its PID
unless its track retires.

What `--sync` does today:

- The tables are laid once, from the catalog the leg opened on. A track
  added afterward fails the export ("… under --sync").
- A track that leaves the catalog keeps its PID and its lane, and an entry
  or `programDescriptors` change is not announced.
- A `muxRate` change is warned and ignored: the leg keeps the rate its grid
  was laid at.
- So legs that open on either side of a catalog change lay different PMTs
  and grids, and a pair does not survive an added stream.

Decided 2026-10-22 by the maintainer: under `moq export ts --sync`, the
revision is a function of the catalog alone, so identical legs revise
their PMT identically. A revision is one transaction over everything the
catalog configures: a `muxRate` change applies with the tables at the same
boundary and, under `--sync`, rebuilds the grid there.

Open: where a revision lands. Each leg reads the catalog update at its own
moment, so the slot that carries the revised PMT, and its `version_number`,
must follow from something both legs read alike, such as an added track's
first unit on its lane or the end of a retired track's last group. A
revision that adds a track also needs a lane the rate can carry.

Rust: `rs/moq-mux` TS export (`with_sync`).

Tests: two synced legs, joined apart, across a PMT add, remove, retype,
descriptor change and `muxRate` change, write the same packets from the
revision on, PMTs included; a leg that joins after a revision lays the
revised PMT at the same `version_number`, and the revised grid.

## Related

- [PAT changes](/quest/m1/ts-reliability/pat-lifecycle.md) - a moved PMT PID reaches the export as a new PAT
