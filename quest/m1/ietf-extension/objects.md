# [M] moq-transport object fields through a relay

## Goal

A moq-transport subscriber reading through moq-relay receives each object of
a single-subgroup track with the publisher's object ID, object properties,
and END_OF_GROUP status object. Datagrams past object 0 forward with their
object IDs, properties, and status instead of being dropped. Plain readers are
unchanged: frames in order with gaps invisible, and no properties.

## Plan

Decided 2026-10-13 (see the [epic](/quest/m1/ietf-extension/README.md)).

Model, all crate-private in `rs/moq-net/src/model`:

- Per frame: `skip: u64`, the object ID delta from the previous object minus
  one, or the first object's ID. Also `properties`, a decoded KVP list
  (`Property { id, value: Int(u64) | Bytes }` or similar). Keep a plain frame
  small, for example an `Option<Box<_>>` for the rare extras, and extend the
  model benchmark to show the cost.
- `frame::Info` does not change. Callers outside the crate build it with
  struct literals, so a private field would break them. The fields go
  through a crate-private writer on `group::Producer` and into internal
  frame storage.
- Per group: the END_OF_GROUP status object's position, as a `skip` from the
  last object.
- Keep the public, exhaustive `Datagram` unchanged so downstream struct
  literals keep compiling. Store its object ID, properties, and END_OF_GROUP
  status in a crate-private wrapper carried by internal track storage and
  relay accessors; public writers use default metadata and public readers
  receive only the original `Datagram`.

IETF ingest and egress in `rs/moq-net/src/ietf`:

- `next_object_id` stops refusing gaps. A stream that starts past object 0,
  or with FIRST_OBJECT clear, is accepted as the subgroup's start when no fill
  or resume is pending. Egress writes the stored delta and sets FIRST_OBJECT
  from the stored state, instead of `slice.skip == 0`.
- Properties are decoded into the list, apart from those already mapped. The
  timestamp still becomes `frame.timestamp`, and egress re-encodes the list
  for the negotiated draft, because KVP encoding differs across drafts 14-22.
  Immutable Properties (0x0B) keep their inner bytes verbatim. The relay adds
  or removes nothing, because moq-test fails on a changed extension count.
- Preserve the ingress encoding family with immutable property bytes.
  Refuse forwarding them between absolute-key drafts 14/15 and delta-key
  drafts 16+, rather than rewriting authenticated bytes or changing their
  meaning. The private lite carrier preserves this provenance too.
- An END_OF_GROUP status object is written where ingest saw one. Otherwise
  egress ends the group with FIN and the header bit, as today.
- Datagrams past object 0 are mapped onto the same model, not dropped.
- FETCH serves a group only if every object ID jump in it is marked by a
  Prior Object ID Gap property. Anything else gets `NOT_SUPPORTED`. FETCH
  objects carry their properties.

Plain readers (the public API, and lite egress without the extension) read
frames in order. Skip and properties never surface.

Docs inline in `doc/concept/standard.md`:

- The "Unknown object properties are dropped" deviation now applies only to
  plain moq-lite and the public API.
- Update the datagram bullet.
- Gaps no longer refuse a group.

Tests: a regression per feature through the IETF dispatch loop, moq-transport
publisher -> relay -> moq-transport subscriber. Cover draft 22 and one older
draft with a different KVP encoding. Shape the cases after moq-test:
increments of 2 and 3, a start object above 0, integer and bytes extensions,
an END_OF_GROUP marker, and datagrams in mode 3.
Also cover immutable byte preservation within an encoding family and
refusal across the draft-15/16 boundary in both directions, using a nested
list with multiple property types.

Public API: none. Wire: none new; moq-transport fields that were dropped are
now forwarded.

## Related

- [IETF group ingest](/quest/m1/ietf-ingest-refusal.md) - narrows the catch-all on the same receive paths; land either first and rebase
