# [S] JS subgroup heads race the subscription closing

## Goal

In `@moq/net`'s IETF subscriber, every await on a subgroup stream before its
group producer exists (the FIRST_OBJECT `peekU62()` and the first
`decodeMaybe`) ends as soon as the subscription closes. A stalled publisher no
longer keeps the Reader, its handler, and the Tail entry alive until the
stream moves or the session closes.

## Plan

Found by the review of [#5069](https://github.com/moq-dev/moq/pull/5069)
(the FIRST_OBJECT backport). Rust's `recv_group` already races these reads
against the subscription; mirror it in `js/net/src/ietf/subscriber.ts`.

Decided 2026-10-08: `main` only, no `release` backport. Only a stalled
publisher is affected, and closing the session clears it.

Test: a publisher opens a subgroup stream and stalls before its first object;
unsubscribing releases the reader and its tail entry without waiting on the
stream.

Public API: none. Wire: none.
