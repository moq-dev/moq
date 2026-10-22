# [M] The IETF answerer pauses any subscription

## Goal

On drafts 18 and later, the Rust moq-transport answerer serves a SUBSCRIBE
with FORWARD=0 paused, and pauses or resumes any Established subscription
when a REQUEST_UPDATE sets FORWARD, as it already does for a PUBLISH it made
for SUBSCRIBE_TRACKS. Draft-22 section 3.1.1 lets the initiator set FORWARD on
SUBSCRIBE and the subscriber change it with REQUEST_UPDATE; today we refuse
both with NOT_SUPPORTED.

This is what a clean Rust-to-Rust IETF listing needs
([moq-dev/moq#6280](https://github.com/moq-dev/moq/pull/6280)): a listed
track held through our own SUBSCRIBE pauses once its last reader leaves, and a
skipped track is listed through a paused SUBSCRIBE. Against a Rust answerer
both fail the listing today, so readers list again, and a track skipped at
the answerer's subscription cap is never listed at all.

## Plan

- Reuse what the paused PUBLISH already does in
  `rs/moq-net/src/ietf/publisher/tracks.rs` (pause, resume with the update's
  filter and fill, answer with LARGEST_OBJECT, a joining FETCH refused while
  paused) rather than a second implementation for SUBSCRIBE.
- A paused subscription forwards nothing, so a hostile subscriber can park
  many: count each against the same subscription slots a forwarding one
  takes.
- Drafts 14-17 keep refusing FORWARD=0; check what each draft allows before
  widening it.
- Test over drafts 18-22 on mock time, including the outbound listing from
  #6280: a SUBSCRIBE-held listed track pauses with its listing intact, and a
  skipped track lists.

## Related

- [Broadcast archives](/quest/m1/archive/README.md) - lists an IETF peer's tracks through SUBSCRIBE_TRACKS
