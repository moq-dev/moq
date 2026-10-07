# [S] CMAF decoders time samples from the frame timestamp

## Goal

A CMAF fragment decodes at its moq-lite frame timestamp, even when its `tfdt`
says otherwise. Today both decoders take sample times from `tfdt` and ignore
the frame timestamp: `fmp4::decode` in `rs/moq-mux/src/container/fmp4/mod.rs`
and `Format.decode` in `js/hang/src/container/cmaf/format.ts`. A publisher
that moves a passthrough track onto another timeline (an importer joining a
shared clock, ad insertion splicing unrelated PTS bases) would play at the
source's PTS.

## Plan

Decided (2026-10-01): the frame timestamp is the broadcast timeline. A
payload's own timestamps are only relative within the frame. Rewriting `tfdt`
on the publisher was rejected: it can force a v0 to v1 box upgrade, which
shifts `trun` data offsets.

- A sample's time is the frame timestamp plus its PTS offset from the
  fragment's first sample. The encoder already stamps the frame with that
  first sample's PTS (`fmp4::encode`), so today's output decodes unchanged.
- Rust's decode gets the frame timestamp from the caller. The fMP4 exporter
  and moq-hls re-fragment decoded frames, so they follow without changes;
  confirm with a test.
- Check the TS exporter's verbatim carriage, which keeps PES PTS inside the
  payload. Rewrite those from the frame timestamp, or refuse a mismatch.
- An untimed CMAF frame decodes at its `tfdt` (decided 2026-10-05, replacing
  the 2026-10-02 decision to refuse it). Rejected: refusing it, and parking
  the quest.
- `main`: a behavior fix with no API change.

Open PR [#4826](https://github.com/moq-dev/moq/pull/4826) implements this. It
lands after [#4822](https://github.com/moq-dev/moq/pull/4822) (the untimed
model), which brings untimed frames to `main`. Timedness is per track there, so the decoder can check the track
rather than each frame.

Test: in Rust and JS, a fragment whose `tfdt` disagrees with its frame
timestamp decodes at the frame timestamp, with B-frame offsets preserved, and
an untimed fragment decodes at its `tfdt`.

Public API: none. Wire: none; this states what the frame timestamp already
means.

## Related

- [Shared clock](/quest/m1/shared-clock.md) - the first publisher to rely on it
