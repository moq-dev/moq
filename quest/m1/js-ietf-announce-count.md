# [S] @moq/net counts an IETF namespace subscription's initial set

## Goal

`@moq/net` speaks the MoQ Namespace Count extension
(`drafts/draft-lcurley-moq-namespace-count.md`) on draft-16+, as `rs/moq-net`
does: it declares the NAMESPACE_COUNT option, its publisher counts the initial
set on the REQUEST_OK answering SUBSCRIBE_NAMESPACE, and its subscriber lands
that stream's source on the count instead of the quiet timer.

## Plan

- Mirror `rs/moq-net/src/ietf/namespace_count.rs` in `js/net/src/ietf/`, like
  `hidden.ts`: declare on draft-16+ only, and read the peer's declaration.
- Publisher: count the first advertised snapshot before writing REQUEST_OK,
  then send exactly those entries first.
- Subscriber: a missing count when negotiated, or one when not, is a protocol
  violation, as in Rust.
- Extend the IETF cases of the JS caught-up tests to show the counted versions
  land without waiting out the quiet gap.

Public API: none. Wire: the extension, already specified.

## Required

- [JS caught up](/quest/m1/js-announce-caught-up.md) - the per-source guard the count lands
