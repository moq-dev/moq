# [S] A mid-group subscription to a compressed track widens to frame 0

## Goal

An application subscription whose range starts mid-group on a compressed
track succeeds: the subscriber's requesting model widens the request to frame
0 of that group before it leaves, and the reader gets the frames it asked for.
Today it fails with `Lagged`. The server never widens and keeps serving
exactly what is asked.

## Plan

Decided with the maintainer on 2026-10-20, from
[#5928](https://github.com/moq-dev/moq/pull/5928)'s Decision 6, which the
[epic](/quest/m1/compress/README.md)'s Ranges rule records: the subscriber
widens, never the server.

- Route-change resume already widens to frame 0. This covers ranges an
  application supplies, through the same requesting-model path rather than a
  second rule.
- Widen only when the subscriber lacks the group's prefix; one that holds
  frames 0..N-1 still asks from N.
- The reader still starts at the frame it asked for: the widened prefix only
  feeds the inflater.

Regression test on mock time, in Rust: a mid-group application subscribe on a
compressed track returns the requested frames instead of `Lagged`, and the
wire request starts at frame 0. Mirror it in `js/net` if
[JavaScript](/quest/m1/compress/js.md) has landed compressed tracks by then.

Public API: none. Wire: none.
