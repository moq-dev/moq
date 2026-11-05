# [M] Our TRACK_STATUS and FETCH carry the request token

## Goal

Our own TRACK_STATUS, standalone FETCH, and a relay's cache-miss group FETCH
carry the connection's request token when one is set, as our SUBSCRIBE and
PUBLISH_NAMESPACE do, so a peer that grants authority per request admits
them.

## Plan

A [moq-dev/moq#5148](https://github.com/moq-dev/moq/pull/5148) follow-up.
The incoming half, a token admitting TRACK_STATUS and FETCH, landed with
[moq-dev/moq#7211](https://github.com/moq-dev/moq/pull/7211), which honors a
token on any request type. This quest keeps only the outgoing half.

Decided 2026-10-20 (maintainer): stays in m3 with the other request-token
surfaces until a consumer asks. Outgoing tokens wait for `add()` with
automatic naming, so this lands with or after that. A joining FETCH keeps
following its subscription's authorization and carries no token of its own.

Tests: outgoing TRACK_STATUS and FETCH carry the token. Run cross-language
interop for the wire change.

Public API: none. Wire: the existing AUTHORIZATION TOKEN parameter (0x03),
now sent on TRACK_STATUS and FETCH.
