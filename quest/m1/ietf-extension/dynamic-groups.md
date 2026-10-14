# [M] Relay requests for dynamic groups

## Goal

A moq-transport subscriber can request a new group from an original
publisher advertising DYNAMIC_GROUPS through moq-relay. Concurrent
subscribers' redundant requests coalesce instead of repeatedly starting
groups. The public API and plain moq-lite behavior stay unchanged.

## Plan

Decided with the maintainer on 2026-10-13: support dynamic groups instead of
refusing otherwise usable tracks that advertise them. This is relay
support, not an application API for requesting or producing keyframes.

- Decode and retain NEW_GROUP_REQUEST (0x32) on SUBSCRIBE and REQUEST_UPDATE
  on the drafts that support it. Interpret DYNAMIC_GROUPS (0x30) as a
  capability, not opaque metadata. Validate its boolean value, including
  when it appears inside Immutable Properties.
- Use a crate-private request path through the track model to the upstream
  subscription driver. Keep capability and request state out of public
  struct literals. Local publishers do not advertise support they cannot
  handle, and no callback API is added.
- Follow the negotiated draft's relay rules. On draft 22, include the
  request when subscribing upstream without an established subscription.
  For an established subscription, send REQUEST_UPDATE for value 0 or a
  value greater than the largest known group when dynamic groups are
  supported. Suppress a nonzero value at or below the largest group, and a
  request already covered by an outstanding greater-or-equal value.
- A request remains outstanding until the largest group increases. ACKing
  REQUEST_UPDATE is not evidence that a new group exists. Do not invent a
  new group ID or create groups in the relay.
- Clear pending state with its subscription, so a reconnect or publisher
  replacement cannot inherit a stale request. Keep this separate from
  historical group fetches.
- Plain lite does not advertise dynamic-group support. The
  [relay extension](/quest/m1/ietf-extension/lite.md) owns carrying capability
  and requests across lite-07 cluster hops.
- Update the deviations in `doc/concept/standard.md` inline. No new user
  guide is needed for relay-only behavior.

Tests through the IETF dispatch loop cover a request on the initial
subscription, an established update, value 0, stale values, unsupported
publishers, redundant and increasing concurrent requests, progress
rearming requests, and subscription teardown. Use simulated time, and wire
the tests into the existing CI suite. Benchmark over tracks and downstream
subscribers so aggregation costs are visible on both axes.

Public API: none. Wire: existing moq-transport fields are interpreted and
relayed; no new IETF fields. Cross-relay lite framing belongs to the relay
extension quest.

## Related

- [Track properties](/quest/m1/ietf-extension/track-properties.md) - advertises DYNAMIC_GROUPS only with a working upstream request path
- [Relay extension](/quest/m1/ietf-extension/lite.md) - carries the request through the relay cluster
- [Draft 22 relay rules](https://www.ietf.org/archive/id/draft-ietf-moq-transport-22.html#section-9.20.19) - forwarding, suppression, and outstanding-request lifetime
