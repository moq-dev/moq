# [S] GOAWAY redirects to loopback are refused in every spelling, on main and release

## Goal

A GOAWAY from a non-local relay cannot steer a `redirect: "follow"` client,
and the tokens it carries, to a loopback, private, or link-local address,
whatever literal spelling the URI uses. A hostname that resolves to such
an address is out of scope: classifying DNS results would race the dial's
own lookup. Rust's `is_local` judges a non-special
scheme's host the way the native dial resolves it, as js/net's typed endpoint
does since [moq-dev/moq#8222](https://github.com/moq-dev/moq/pull/8222), and
both fixes reach the `release` line in its next patch.

## Plan

Planned 2026-11-08 (maintainer, approved follow-up of
[moq-dev/moq#8222](https://github.com/moq-dev/moq/pull/8222)). Milestone:
m0, since it is a hostile-input fix in shipped code plus a `release`
backport.

- Rust on `main`: `is_local` in `rs/moq-net/src/reconnect/redirect.rs`
  parses a `Host::Domain` with `str::parse::<IpAddr>`, which refuses
  shorthand and numeric IPv4 (`127.1`, `0x7f000001`, `2130706433`), so
  `moqt://127.1` counts as non-local while the native dial may still reach
  127.0.0.1. Classify the host the dial will use: decode the IPv4 spellings
  WHATWG's host parser accepts (as js/net reads a non-special host back
  through https), and treat a trailing-dot `localhost.` as `localhost`, as
  `js/net/src/connection/endpoint.ts` does.
- Test both sides from one fixture of endpoint spellings and expected
  verdicts, replayed by `rs/moq-net` and `js/net`, so the two classifiers
  cannot drift again. Put it beside the other shared corpora (see
  [one fixture layout](/quest/m4/corpus-fixtures.md)).
- Backport to `release`: js/net's GOAWAY classification
  (`js/net/src/connection/goaway.ts` `isLocal` on the opaque `moql:`/`moqt:`
  host) and Rust's (`rs/moq-tokio/src/connection.rs` `is_local` there) both
  have the hole. Port the smallest fix to each as PRs into `release`; the
  maintainer cuts the patch.

Public API: none. Wire: none. Behavior: such redirects are refused.
