# [S] Every binding documents its connection stats

## Goal

`doc/lib/{c,py,swift,kt,go,dart}` each list the connection stats fields the
binding exposes, with units and validity, so a consumer learns what
`estimated_send_rate_bps` or `rtt` means without reading the generated
record. Today only the Go and C pages name the call, and none lists a field.

## Plan

One section per page, generated from the same source of truth
(`MoqConnectionStats` in `rs/moq-ffi/src/session.rs` and
`moq_connection_stats` in `rs/libmoq/src/api.rs`) in the language's own
casing, with the validity flags the C ABI carries and the estimate names
settled in #3744. A doc check greps each page for every field name in the
Rust struct so a future field cannot land undocumented.

## Required

- [Merge dev](/quest/m1/merge-dev.md) - the estimate names are the ones dev renamed
