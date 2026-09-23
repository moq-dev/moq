# [S] js/net: path-keyed publisher state follows a same-path replacement

## Goal

Replacing a path's producer in `js/net` without closing the predecessor
behaves like a new broadcast. Discovery announces the replacement, `TRACK_INFO`
answers with the successor's metadata over both SUBSCRIBE and FETCH, and an
IETF `PUBLISH_NAMESPACE` refusal against the predecessor no longer suppresses
the successor.

Epochs give each default publish a distinct path, but raw paths (the
prefix-route opt-out) still reuse names, so this still needs its own fix.

## Plan

Follow the local fix the issue proposes: reconcile announce state by producer
identity rather than by key, evict the path's `#trackInfo` entries on
`publish`, and key `refused` by producer identity. The issue lists the
regression tests. Check whether Rust has the same three gaps and fix any it
does in the same PR. Public API: none. Wire: none.

## Closes

- [#2985](https://github.com/moq-dev/moq/issues/2985) - close this issue when the quest finishes

## Related

- [#2991](/quest/m1/2991-net-coalesce-dynamic-tracks-and-preserve-sequences-across.md) - the dynamic-track half of the same replacement identity problem
