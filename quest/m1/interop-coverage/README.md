# Interop coverage for moq-transport

## Goal

The moq-transport features moq-net exposes with the most interop bug
history have cross-implementation tests: Rust, JS, and the bindings exchange
hang media and exercise subgroup data, namespace discovery, late joins,
FETCH, GOAWAY, TRACK_STATUS, and datagrams over the even drafts 14
through 22, and request limits over drafts 14 and 16 (later drafts have
no MAX_REQUEST_ID). Our upstream runner clients implement the runner's
existing cases for those features.

Boundaries:

- Only what moq-net can do. PUBLISH (push), subgroups other than 0,
  outbound forward=0, rendezvous, and per-request authorization tokens are
  not exposed, so they are out of scope, and runner cells that need them
  stay red by design.
- Media stays hang. No MSF catalog in our runner clients.
- New cases live in-tree (`test/interop`), not in the runner's spec.
- Not covered: mid-subscription updates (`Subscription::update` in Rust and
  JS, sent as SUBSCRIBE_UPDATE or REQUEST_UPDATE) for priority, max age,
  NEW_GROUP_REQUEST, and group order. The maintainer did not pick them on
  2026-10-21; plan them separately if their bug history grows.

## Plan

Decided with the maintainer on 2026-10-21, from an inventory of the
coverage on `main` at `3ab6cbf81d`.

- `just test interop --versions ietf` runs the matrix on each even draft;
  every new case runs on it. Beyond it, moq-transport is covered by
  in-process codec tests (`rs/moq-net/src/test_interop.rs`), the public
  relays page (published clients), and the Cloudflare and Moxygen smoke
  lanes.
- Even drafts only (14, 16, 18, 20, 22), nightly.
- Case ranking follows interop bug history: subgroup data and namespace
  discovery, then late join and FETCH, then GOAWAY, TRACK_STATUS,
  datagrams, and limits.

## Required

- [JS namespace lifecycle](/quest/m1/interop-coverage/js-namespace-done.md) - moq-dev-js subscribe-namespace-lifecycle sees NAMESPACE_DONE with publisher and subscriber in one process
- [JS subgroup data through imquic](/quest/m1/interop-coverage/js-imquic-objects.md) - moq-dev-js data-subgroup-basic passes against imquic, or the cell is classified with imquic's split-header bug reported upstream
- [Late join and FETCH](/quest/m1/interop-coverage/late-join.md) - a late subscriber gets the current group from object 0, and a standalone FETCH returns a past group exactly

## Related

- [Runner client bump](/quest/m2/release/runner-client-bump.md) - after the cut publishes #5083, the runner's Rust client stops stamping every object
- [Interop audit](/quest/m1/audit-interop.md) - classifies the red cells these cases produce
- [moq-transport passthrough](/quest/m1/ietf-extension/README.md) - Moxygen's moq-test conformance through moq-relay
