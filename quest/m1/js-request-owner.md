# [S] Every JS request stream has one owner and one closer

## Goal

In `js/net`'s IETF adapter, each request stream has a single handle that
owns its reader and exposes a terminal `ended` promise every consumer races,
so no request reader can be left detached, and each request closes through a
single per-request closer, so request tasks stop closing a session or stream
twice with competing codes.

## Plan

Follow-ups of [#5998](https://github.com/moq-dev/moq/pull/5998). Today no
reader is detached, but nothing prevents it: each request type wires its own
read loop and close path, and #5998's unit harness has to assert that every
close carries one code because the transport keeps the first.

- Add the handle internally in `js/net/src/ietf/` and move SUBSCRIBE, FETCH,
  TRACK_STATUS, SUBSCRIBE_NAMESPACE, SUBSCRIBE_TRACKS and PUBLISH_NAMESPACE
  (both sides) onto it.
- Check whether the Rust request tasks close twice the same way and give
  them the same single closer if so; mirror the shape in both languages.
- Internal refactor: deleting the per-request read and close code it
  replaces is the measure of success.

Public API: none. Wire: none.
