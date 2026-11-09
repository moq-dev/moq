# [S] Backport the GOAWAY loopback refusal to release

## Goal

The `release` line refuses a GOAWAY from a non-local relay that steers a
`redirect: "follow"` client, and the tokens it carries, to a loopback,
private, or link-local address in any literal IPv4 spelling (`127.1`,
`0x7f000001`, `2130706433`, `0177.0.0.1`) or a trailing-dot `localhost.`,
in both Rust and js/net, so the next patch ships the fix `main` has.

## Plan

`main` fixed both classifiers and shares their cases in
`rs/moq-net/fixtures/endpoints.json`. Port the smallest fix to `release` as
PRs into `release`, keeping the published API (CONTRIBUTING.md); the
maintainer cuts the patch.

- Rust: `is_local` in `rs/moq-tokio/src/connection.rs` parses a
  `Host::Domain` with `str::parse::<IpAddr>`. Read the host back through an
  `https://` URL, as `main`'s `rs/moq-net/src/reconnect/redirect.rs` does,
  and strip a trailing dot before the `localhost` check.
- JS: `isLocal` in `js/net/src/connection/goaway.ts` reads the opaque
  `moql:`/`moqt:` hostname. Reparse a non-special URL's host through https
  before classifying, as `main`'s `js/net/src/connection/endpoint.ts` does.
- Cover the shorthand spellings in each side's existing tests.

Public API: none. Wire: none. Behavior: such redirects are refused.
