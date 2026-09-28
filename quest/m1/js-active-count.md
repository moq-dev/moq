# [S] @moq/net counts an IETF namespace subscription until it is caught up

## Goal

`@moq/net` speaks the MoQ Active Count extension
(`drafts/draft-lcurley-moq-active-count.md`) on draft-16+, as `rs/moq-net`
does: it declares the ACTIVE_COUNT option, its publisher puts on the REQUEST_OK
answering SUBSCRIBE_NAMESPACE how many NAMESPACE messages come before the
subscription is caught up, and its subscriber lands that stream's source on
the count instead of the quiet timer.

## Plan

- Mirror `rs/moq-net/src/ietf/active_count.rs` in `js/net/src/ietf/`, like
  `hidden.ts`: declare on draft-16+ only, and read the peer's declaration.
- Publisher: count what it advertises before writing REQUEST_OK, then send
  exactly those NAMESPACE messages first.
- Subscriber: a missing count when negotiated, one when not, or one on any
  other REQUEST_OK is a protocol violation, as in Rust.
- Extend the IETF cases of the JS caught-up tests to show the counted versions
  land without waiting out the quiet gap.

Public API: none. Wire: the extension, already specified.
