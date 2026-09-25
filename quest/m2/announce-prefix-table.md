# [L] Bounded announce prefix table

## Goal

MoQ-lite announcement starts can refer to repeated path-segment prefixes,
including a customer PID, using a bounded table scoped to one ordered Announce
Stream. Repeated-name traffic uses fewer actual network bytes without changing
the reconstructed path, route behavior, or the raw path length reported for
usage. The new encoding uses lite-08, while lite-07 and older versions remain
compatible.

## Plan

Extend the MoQ-lite draft and Rust encoder/decoder together. After the
ANNOUNCE_REQUEST and ANNOUNCE_OK exchange, each ordered Announce Stream owns an
initially empty prefix table. Define explicit insert/reference/literal forms
for `ANNOUNCE_START` path suffixes with whole-segment matching. Bound entries
and total bytes, specify deterministic eviction and stream reset, and allow a
literal when the table would not save bytes. Both peers process updates in
stream order, so an announcement never waits on a different stream's state.
Reject invalid references and lengths as protocol violations. Keep
`ANNOUNCE_END` and `ANNOUNCE_UPDATE` on their lite-06 IDs rather than re-sending
the path.

Introduce `moq-lite-08` after lite-07. Keep lite-07 framing intact; do not
silently reinterpret an already negotiated stream. Prove mixed-version peers negotiate a common older version and that a new
stream after reconnect starts with an empty table. Exercise
literal fallback, repeated PID, nested tuple prefixes, table-full eviction,
malformed reference, duplicate route, and interleaved starts/ends in codec and
end-to-end relay tests. Update the MoQ-lite draft and any changed wire examples
in the same PR.

Benchmark encoded control-message bytes and actual QUIC bytes with identical
announcement events and mesh topology, using a path sample shaped like the
health project: many unique timestamp suffixes under repeated
`<pid>/private/channel_.../stream-health-*` prefixes. Report the table hit
rate, network byte delta, CPU time, memory bound, and start/end counts. Compare
against lite-06 ID-based END alone so the table's incremental gain is clear.
Do not require customer clients to use the new version; the first adopter is
the internal moq.pro mesh after lite-06 has rolled out.

## Related

- [Relay memory](/quest/m1/relay-memory.md) - route state footprint, which
  path encoding does not remove
- [PoP skipping](/quest/m1/pop-skipping/README.md) - the lite-06 rollout
  work that must complete before moq.pro activates this encoding
