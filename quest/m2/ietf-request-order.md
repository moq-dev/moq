# [S] IETF requests take their ID in send order

## Goal

On moq-transport drafts 14 and 15, a request never spends an ID it cannot
send, legacy request paths have no `control.sending()` races, and an outbound
SUBSCRIBE_NAMESPACE never closes the session over its own local close.

## Plan

Decided 2026-10-21 with the maintainer: m2, since legacy IETF drafts are low
priority.

- **ID order.** Rust `advertise_namespace` takes its request ID before
  opening the stream. On a d14/15 timeout that leaves a gap, and a peer that
  requires the next ID refuses every later request. Open first, then take
  the ID.
- **`control.sending()` races.** Audit the other legacy paths for the same
  shape ([moq-dev/moq#6126](https://github.com/moq-dev/moq/pull/6126)).
- **SUBSCRIBE_NAMESPACE self-close.** A local close during the counted
  initial set may make the read loop see an end and throw "namespace stream
  ended inside the initial set", closing the session over our own close
  ([moq-dev/moq#6130](https://github.com/moq-dev/moq/pull/6130)).
  Unverified: reproduce it before fixing, and drop the bullet if it does not
  reproduce.

Each fix lands with a mocked-time regression test on the affected drafts.

Public API: none. Wire: none.

## Related

- [JS IETF request streams](/quest/m2/js-request-stream.md) - the JS side of the legacy request cleanup
