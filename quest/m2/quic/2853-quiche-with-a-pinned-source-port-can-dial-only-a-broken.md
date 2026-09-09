# [S] quiche with a pinned source port can dial only a broken IPv4 address

## Goal

A quiche dial with a pinned non-zero `--connect-bind` port waits for the
authoritative all-family DNS answer before taking its single candidate, so a
host with a slow AAAA and a broken IPv4 path still connects over IPv6.

## Plan

`rs/moq-tokio/src/quiche.rs:408-410` truncates the candidate list to one
address when the source port is pinned, because a pinned port only fits one
socket at a time:

```rust
if self.bind.port() != 0 {
    candidates = candidates.with_limit(1);
}
```

Since #2749, that single candidate can come from the speculative IPv4-only
lookup rather than the authoritative all-family answer. `Candidates::next`
already mitigates this per RFC 8305 section 3: for the first candidate it
holds an IPv4-only answer back and waits up to `--connect-resolution-delay`
(50ms) for the full lookup (tests
`ipv4_waits_out_the_resolution_delay_for_the_full_answer` and
`ipv4_proceeds_once_the_resolution_delay_expires`,
rs/moq-tokio/src/resolve.rs:731, :754), so the platform's own RFC 6724
ranking usually wins.

That bounds the window rather than closing it. If AAAA is more than the
resolution delay slower than A, the wait times out and the IPv4 address is
taken. With `limit(1)` there is no second attempt, so a host whose IPv4 path
is broken and whose IPv6 path works fails to connect, where before #2749 the
dial waited for the complete resolver result and took its first (IPv6)
address.

Narrow by construction: it needs the quiche backend, a pinned non-zero
`--connect-bind` port, a slow AAAA, and a broken IPv4 path. Every other
backend races both families, so `limit(1)` is the only place a preference
becomes an exclusion.

The fix is one branch plus a test: on the `limit(1)` path, wait for the
authoritative answer rather than accepting the fast lane. The fast lane
exists to start dialing sooner, which is worth nothing when only one attempt
will ever be made. The code is identical on `main`
(rs/moq-native/src/quiche.rs:339), so the fix lands on either branch.

## Required

- [noq parity gate](/quest/m2/quic/noq-parity.md) - decides whether quiche stays a supported backend; if it is retired this quest is abandoned with the verdict

## Closes

- [#2853](https://github.com/moq-dev/moq/issues/2853) - close this issue when the quest finishes
