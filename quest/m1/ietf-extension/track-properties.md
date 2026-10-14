# [S] Forward unknown moq-transport track properties

## Goal

Through moq-relay, a moq-transport subscriber receives the publisher's track
properties that moq-net does not understand, unchanged in SUBSCRIBE_OK,
FETCH_OK, and TRACK_STATUS_OK. The properties moq-net already understands are
handled as today.

## Plan

Decided 2026-10-13 (see the [epic](/quest/m1/ietf-extension/README.md)).

- Store the unknown properties crate-private on `track::Info`, which is
  `#[non_exhaustive]`, using the `Property` list from
  [object fields](/quest/m1/ietf-extension/objects.md). Re-encode them for each
  egress draft. Immutable Properties keep their inner bytes verbatim and
  their ingress encoding family; the cross-family refusal from object
  fields applies to track properties too.
- Unknown properties include those the draft defines but moq-net does not
  implement, such as OBJECT_DELIVERY_TIMEOUT and SUBGROUP_DELIVERY_TIMEOUT.
  They are forwarded, not interpreted. DYNAMIC_GROUPS is handled by
  [dynamic groups](/quest/m1/ietf-extension/dynamic-groups.md), so it is
  advertised only when the upstream request path supports it.
- An unknown Mandatory Track Property (0x4000-0x7FFF) refuses the track with
  UNSUPPORTED_EXTENSION, as the draft requires, instead of forwarding it.
- Group order stays as today. The model owns serving order.
- Docs inline in `doc/concept/standard.md`.

Tests: an unknown even and an unknown odd property cross the relay unchanged
on draft 22 and one older draft, and an unknown mandatory property refuses
the track.

Public API: none. Wire: none new.

## Required

- [Object fields](/quest/m1/ietf-extension/objects.md) - introduces the decoded `Property` list
- [Dynamic groups](/quest/m1/ietf-extension/dynamic-groups.md) - makes DYNAMIC_GROUPS advertising truthful by carrying NEW_GROUP_REQUEST upstream
