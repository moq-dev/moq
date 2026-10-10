# [S] moqsink's first catalog lists every requested pad

## Goal

When a `moqsink` run starts, its first catalog lists every pad requested so
far, not only the pads whose caps arrived first. A `moqsrc` following a
restarted `moqsink` then resumes each rendition on its held pad, instead of
ending a late-listed one with EOS and adding a new, unlinked pad for it. A
rendition the new run really drops still ends with EOS.

Non-goals: other publishers, whose first catalog is already complete
(`@moq/publish` waits for its tracks to settle, and the CLI importers reserve
their track set up front).

## Plan

Decided 2026-10-10, planning the follow-ups of
[#5181](https://github.com/moq-dev/moq/pull/5181):

- The fix lives in `moqsink`, and `moqsrc`'s rule stays: a rendition the new
  run's first catalog does not list ends with EOS. Holding unlisted
  renditions in `moqsrc` instead would let one that never returns stall a
  downstream muxer, and every other publisher already sends a complete first
  catalog.
- Each pad reserves its catalog slot (`moq_mux::catalog::Producer::reserve`)
  when it is requested in `request_new_pad` (`rs/moq-gst/src/sink/imp.rs`),
  instead of when its caps arrive (`catalog.reserve()` in
  `rs/moq-gst/src/sink/pad.rs`), hands it to its importer once the caps
  resolve the rendition, and drops it if the pad is released first. The
  catalog withholds its first snapshot until every reservation resolves, so
  that snapshot lists every pad. Each run re-reserves for the pads that exist
  when it starts.

Open, for the maintainer:

- A pad requested but never fed holds back the whole catalog; opaque pads
  never reserve for this reason (`an_opaque_pad_stays_out_of_the_catalog`).
  Recommended: drop the reservation when the pad is released or reaches EOS
  without caps, and document that a linked pad must negotiate before the
  broadcast lists anything. Alternative: drop every reservation still without
  caps once another pad's first frame is ready, so the catalog waits only for
  pads negotiating together, at the cost of the race this quest removes.
- Milestone: m1 is the recommendation, since only a restart of a `moqsink`
  whose pads negotiate at different times is affected. Alternative: m0 under
  [Broadcast epochs](/quest/m0/broadcast-epoch/README.md), gating the release.
- Docs: the `moqsink` and `moqsrc` paragraphs of `doc/bin/gstreamer.md`
  describe the rule. Whether anything beyond them is needed is open;
  recommended no.

Tests: a `moqsink` whose audio pad gets caps after its video pad's first
buffer publishes one first catalog listing both; and a `moqsrc` following
that `moqsink` across a restart keeps both pads without EOS.

The comment above the catalog subscribe in `follow_catalog`
(`rs/moq-gst/src/source/imp.rs`) says the browser announces an empty catalog
before its encoder configures. Check it against `@moq/publish` and correct it
if stale.

## Required

- [moqsrc follows a restart](/quest/m0/broadcast-epoch/moqsrc.md) - the restart hold this protects, and the end-to-end test

## Related

- [#3115](/quest/m2/3115-moqsink-the-publication-has-no-generation-so-a-flush.md) - a new generation re-reserves for its pads too
