# [XS] Document the relay's moq-transport deviations

## Goal

`doc/concept/standard.md` lists, under moq-transport, the deliberate
deviations that Fastly's moq-relay-interop report flagged, each with its
reason and draft section, so the next interop run has something to point at:

- One publisher per broadcast path: a SUBSCRIBE goes to one route, not to
  every matching publisher (d16 §8.4), because a path names one content.
- Unknown object properties are dropped, not forwarded (d18 §2.5), because
  the model carries only payload and timestamp.
- A source with no track stream (lite-01 to 04) behind the relay gets
  SUBSCRIBE_OK before the source answers, so a missing track ends as
  PUBLISH_DONE, not REQUEST_ERROR.

## Plan

Triaged on 2026-10-07. Decided then: these are the product's model (see
[A gapped IETF object ID](/quest/m1/ietf-object-gaps.md) and the AGENTS.md
rule that a name always means the same content), so they are documented,
not changed. Check the wording against `doc/concept/moq-lite.md`, which
already explains what moq-lite leaves out, and link to it rather than
repeat it.

Public API: none. Wire: none.
