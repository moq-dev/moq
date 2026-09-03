# [M] quiche with a pinned source port can dial only a broken IPv4 address

## Goal

Implement and verify the behavior tracked in [#2853](https://github.com/moq-dev/moq/issues/2853)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

#### Problem

`rs/moq-native/src/quiche.rs` truncates its candidate list to one address when `--connect-bind` pins a non-zero source port, because a pinned port only fits one socket at a time:

```rust
if self.bind.port() != 0 {
    candidates = candidates.with_limit(1);
}
```

Since #2749, that single candidate can come from the speculative IPv4-only lookup rather than the authoritative all-family answer. `Candidates::next` already mitigates this per RFC 8305 section 3: for the first candidate it holds an IPv4-only answer back and waits up to `--connect-resolution-delay` (50ms) for the full lookup, so the platform's own RFC 6724 ranking usually wins.

That bounds the window rather than closing it. If AAAA is more than the resolution delay slower than A, the wait times out and the IPv4 address is taken. With `limit(1)` there is no second attempt, so a host whose IPv4 path is broken and whose IPv6 path works now fails to connect, where before #2749 the dial waited for the complete resolver result and took its first (IPv6) address.

Narrow by construction: it needs the quiche backend, a pinned non-zero `--connect-bind` port, a slow AAAA, and a broken IPv4 path. Every other backend races both families, so `limit(1)` is the only place a preference becomes an exclusion.

#### Suggested direction

For the `limit(1)` path specifically, wait for the authoritative answer rather than accepting the fast lane: the fast lane exists to start dialing sooner, which is worth nothing when only one attempt will ever be made.

Found while reviewing #2852 (merging main into dev); the code is identical on `main`, so this is not a merge regression and was left out of that PR. Originally raised by the Codex connector bot as an inline comment there.

## Closes

- [#2853](https://github.com/moq-dev/moq/issues/2853) - close this issue when the quest finishes
