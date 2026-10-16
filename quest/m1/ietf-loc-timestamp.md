# [XS] Read only the current LOC Timestamp ID

## Goal

Rust and JS read a moq-transport object's Timestamp only from the current LOC
property ID (0x10). Property 0x06 is opaque on every draft, never read as the
draft-ietf-moq-loc-03 Timestamp.

## Plan

Decided with the maintainer on 2026-10-15: we support only the latest LOC
Timestamp ID. LOC and moq-transport are versioned independently, so a LOC ID
must not depend on the transport draft. Today 0x06 is read as the loc-03
Timestamp only before draft 19, keyed on the transport version, because
draft 19 reassigns 0x06 to SUBGROUP_DELIVERY_TIMEOUT.

Delete the loc-03 decode and everything that exists only for it:

- `PROP_TIMESTAMP_DRAFT03` and its decode in `rs/moq-net/src/ietf/`.
- The unit conversion of 0x06 when relaying into an egress with different
  units, and its drop for a unitless egress (from
  [#5494](https://github.com/moq-dev/moq/pull/5494)).
- `hasDraft03Timestamp` in `js/net/src/ietf/object.ts`.
- The tests for these paths.

Keep the draft-19 boundary guard: a mutable 0x06 is dropped and a sealed one
refused when relaying between drafts 18 and earlier and drafts 19 and later.
That guard is about moq-transport reassigning the ID, not about LOC. Without
it, a draft-18 publisher's 0x06 would arrive downstream as
SUBGROUP_DELIVERY_TIMEOUT. Below and above the boundary, 0x06 is forwarded
like any other unknown property.

Public API: none. Wire: a peer still writing 0x06 Timestamps reads as untimed.

## Related

- [moq-transport passthrough](/quest/m1/ietf-extension/README.md) - carries object properties through relays
