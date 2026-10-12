# [S] moqsink's first catalog lists every requested pad

## Goal

When a `moqsink` run starts, its first catalog lists every pad requested so
far, not only the pads whose caps arrived first. A `moqsrc` following a
restarted `moqsink` then resumes each rendition on its held pad, instead of
ending a late-listed one with EOS and adding a new, unlinked pad for it. A
rendition the new run really drops still ends with EOS.

Non-goals: other publishers. The CLI importers reserve their track set up
front, and `<moq-publish>`'s default `source` mode waits for its enabled
tracks to settle before announcing. `@moq/publish` with `announce="always"`,
or its `Broadcast` class used directly, announces before its renditions
resolve and has the same gap, which is planned separately.

## Plan

Decided 2026-10-10, planning the follow-ups of moqsrc's restart work
([#5181](https://github.com/moq-dev/moq/pull/5181), superseded by
[#5191](https://github.com/moq-dev/moq/pull/5191)):

- The fix lives in `moqsink`, and `moqsrc`'s rule stays: a rendition the new
  run's first catalog does not list ends with EOS. Holding unlisted
  renditions in `moqsrc` instead would let one that never returns stall a
  downstream muxer.
- A browser publisher that announces before its tracks settle stays out of
  scope (decided in #5189's review), so this quest stays a `moqsink` change.
- Each pad reserves its catalog slot (`moq_mux::catalog::Producer::reserve`)
  when it is requested in `request_new_pad` (`rs/moq-gst/src/sink/imp.rs`),
  instead of when its caps arrive (`catalog.reserve()` in
  `rs/moq-gst/src/sink/pad.rs`), hands it to its importer once the caps
  resolve the rendition, and drops it if the pad is released first or its
  caps are opaque, which the catalog never lists. The catalog withholds its
  first snapshot until every reservation resolves, so that snapshot lists
  every pad. The catalog producer exists only during a run (`READY` to
  `PAUSED`), so each run reserves for the pads that exist when it starts
  (`start_session`), and a pad requested mid-run reserves at once.
- A pad admitted while a run starts must not miss the gate. `request_new_pad`
  counts the admission under the control lock before `add_pad`, so
  `start_session` can run in between and miss the pad in `sink_pads`, and a
  sibling can publish the first snapshot before the pad confirms. So key the
  slot by pad, not by a count: each pad's lifecycle holds at most one
  reservation, and filling it is idempotent. The control state tracks
  admitted-but-unconfirmed pads instead of only `control.admissions`;
  `start_session` fills the slot of every pad in `sink_pads` or that set, a
  mid-run request fills it when it is admitted, and `cancel_admission` or
  release drops it. A pad already visible in `sink_pads`, or whose CAPS
  arrived through a reentrant `child_added` handler, is then never reserved
  twice.

Decided 2026-10-11 by the maintainer, in #5189:

- A pad requested but never fed holds back the run's first catalog, so its
  reservation drops when the pad is released, reaches EOS without caps, or
  gets opaque caps. Every other terminal path before the rendition resolves
  drops it too: caps that fail (`CapsOutcome::Failed`, such as AAC without
  `codec_data` or a duplicate track name), and EOS after caps but before the
  importer resolved a config (H.264 byte-stream that ends before its SPS),
  whose importer owns the reservation by then. Today `handle_event(Eos)`
  only marks the pad ended, and producers finalize once every pad ends, so a
  streaming sibling would keep the gate shut. Dropping reservations once
  another pad's first frame is ready was rejected: it brings back the race
  this quest removes. The initial gate only holds the first snapshot, so
  later catalogs never wait.
- m1, ranked after #3021: only a restart of a `moqsink` whose pads negotiate
  at different times is affected, so it does not gate the release under
  [Broadcast epochs](/quest/m0/broadcast-epoch/README.md).
- Docs stay inline: the implementing PR updates the `moqsink` and `moqsrc`
  paragraphs of `doc/bin/gstreamer.md`, including that every linked pad must
  negotiate before the broadcast lists anything. No separate docs quest.

Tests: a `moqsink` whose audio pad gets caps after its video pad's first
buffer publishes one first catalog listing both; a pad that is released,
ends without caps, fails its caps, or ends after caps but before its config
lets the first catalog publish while a sibling streams; a pad requested while
the run starts is listed even when a sibling resolves before that pad
confirms, and reserved once even when it is already in `sink_pads`; and a
`moqsrc` following that `moqsink` across a restart keeps both pads without
EOS.

## Related

- [#3115](/quest/m2/3115-moqsink-the-publication-has-no-generation-so-a-flush.md) - a new generation re-reserves for its pads too
