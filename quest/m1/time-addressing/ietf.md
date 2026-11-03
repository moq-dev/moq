# [S] moq-transport carries group IDs verbatim and gaps best-effort

## Goal

moq-net keeps a moq-transport group ID verbatim as the group's key in both
directions. It sends each group's predecessor gap in Prior Group ID Gap as a
best-effort drop signal, and marks a whole group with END_OF_GROUP. It sends
no Timestamp object property of its own. LOC object properties are proxied
only between two moq-transport peers.

## Plan

Decided 2026-11-03 (maintainer, the `/quest-import` grill of
[moq-dev/moq#7573](https://github.com/moq-dev/moq/issues/7573)), replacing
the 2026-10-25 plan that carried each object's PTS in TIMESTAMP (0x10) with
TIMESCALE (0x08):

- **The Group ID is the key**, verbatim on ingest and egress
  ([the model](/quest/m1/time-addressing/model.md) owns that mapping). A
  moq-transport track is in whatever unit its IDs are in; nothing infers
  time from its objects.
- **No Timestamp property from us.** moq-net stops sending TIMESTAMP on
  egress and doesn't lift it into the frame on ingest. LOC object properties
  pass through unchanged when both sides speak moq-transport, and are dropped
  toward moq-lite. A moq-transport publisher's LOC media relayed to a
  moq-lite viewer therefore arrives without a frame PTS, which is accepted.
  Today's TIMESTAMP code (`ietf/group.rs`, `ietf/object.rs`,
  `ietf/max_age.rs`) is deleted or reduced to that pass-through.
- **Prior Group ID Gap stays** as a best-effort SUBSCRIBE_SKIP signal:
  egress sends the gap to the previous key, and ingest reads it as the
  skipped IDs never produced. A peer that sends none still works.
- **GROUP_END maps to END_OF_GROUP.** The supported drafts refuse properties
  on a status object (`check_status` in `rs/moq-net/src/ietf/object.rs`), so
  END_OF_GROUP stays bare and the end time is lost on this wire.
- Object IDs stay +1 per frame, so no Prior Object ID Gap is sent.

Decision prompts:

- Timestamp property: ✅ stop, with LOC proxied only between moq-transport
  peers; keep reading but stop sending; keep the 2026-10-25 plan.
- Prior Group ID Gap: drop it; ✅ keep it as a best-effort SUBSCRIBE_DROP
  signal (a SUBSCRIBE_SKIP signal since lite-07 split never-produced holes
  out, 2026-11-03).

Test both directions over drafts 18, 20, and 22: IDs round-trip exactly
through a relay, a gap reaches the subscriber as a drop, and LOC properties
survive moq-transport to moq-transport.

Public API: none. Wire: moq-transport loses our TIMESTAMP property, and
gains Prior Group ID Gap and END_OF_GROUP on whole groups.

## Required

- [Rust groups by start](/quest/m1/time-addressing/model.md) - the model whose keys and gaps these carry
