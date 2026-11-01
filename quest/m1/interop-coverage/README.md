# Interop coverage for moq-transport

## Goal

The moq-transport features moq-net exposes with the most interop bug
history have cross-implementation tests: Rust, JS, and the bindings exchange
hang media and exercise subgroup data, namespace discovery, late joins,
FETCH, GOAWAY, TRACK_STATUS, and datagrams over drafts 18 and 22. Our
upstream runner clients implement the runner's existing cases for those
features.

Boundaries:

- Only what moq-net can do. Subgroups other than 0, rendezvous, and
  per-request authorization tokens are not exposed, so they are out of
  scope, and runner cells that need them stay red by design.
- PUBLISH (push) and outbound forward=0 are out of scope (decided
  2026-10-25, maintainer, reversing the 2026-10-22 decision): no child
  quest owned them, and the cases above carry the interop bug history.
- Media stays hang. No MSF catalog in our runner clients.
- New cases live in-tree (`test/interop`), not in the runner's spec.
- Not covered: mid-subscription updates (`Subscription::update` in Rust and
  JS, sent as SUBSCRIBE_UPDATE or REQUEST_UPDATE) for priority, max age,
  NEW_GROUP_REQUEST, and group order. The maintainer did not pick them on
  2026-10-21; plan them separately if their bug history grows.

## Plan

Decided with the maintainer on 2026-10-21, from an inventory of the
coverage on `main` at `3ab6cbf81d`.

- `just test interop --versions ietf` runs the matrix on each supported draft;
  every new case runs on it. Beyond it, moq-transport is covered by
  in-process codec tests (`rs/moq-net/src/test_interop.rs`), the public
  relays page (published clients), and the Cloudflare and Moxygen smoke
  lanes.
- Drafts 18 and 22 only, nightly: the only drafts left once
  [moq-transport 18 and 22 only](/quest/m1/ietf-drop/README.md) lands
  (decided 2026-10-24, maintainer). The request-limits case went with
  drafts 14 and 16, the only ones with MAX_REQUEST_ID.
- Case ranking follows interop bug history: subgroup data and namespace
  discovery, then late join and FETCH, then GOAWAY, TRACK_STATUS,
  and datagrams.

## Required

- [Draft-18 TRACK_STATUS FIN](/quest/m1/interop-coverage/draft18-status-fin.md) - a draft-18 TRACK_STATUS followed at once by a FIN is answered, so moqx-forwarded status requests reach moq-dev publishers
- [JS namespace lifecycle](/quest/m1/interop-coverage/js-namespace-done.md) - the runner's moq-dev-js client moves to an `@moq/net` that withdraws namespaces by reset, so subscribe-namespace-lifecycle passes on Cloudflare draft-18 and aiomoqt
- [JS subgroup data through imquic](/quest/m1/interop-coverage/js-imquic-objects.md) - the runner's moq-dev-js client moves to an `@moq/net` patch that writes data stream headers in one write, so data-subgroup-basic passes against imquic
- [JS subscribers FETCH](/quest/m1/interop-coverage/js-ietf-fetch.md) - `@moq/net` fetches past groups over moq-transport, and joins the CLI FETCH round

## Related

- [Runner client bump](/quest/m2/release/runner-client-bump.md) - after the cut publishes #5083, the runner's Rust client stops stamping every object
- [Interop audit](/quest/m2/audit-interop.md) - classifies the red cells these cases produce
