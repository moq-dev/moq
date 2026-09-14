# [L] moq-auth owns the contract and the token

## Goal

`rs/moq-auth` and `js/auth` (`@moq/auth`) exist and hold everything a party
needs to ask for or answer an authorization: the request and grant schemas,
the lease split the relay consumes, the HTTP client that drives it, and the
JWT claims, keys, signing, and verification that `rs/moq-token`,
`rs/moq-token-cli`, and `js/token` held until now. Those three are deleted,
`moq auth generate|sign|verify` replaces `moq token` and the standalone
`moq-token` binary, and claims are pattern unions with no prefix mode. Every
consumer in the repository builds against the new crate and package. On dev.

## Plan

The contract is in the [questline](/quest/m1/auth/README.md); this quest
lands the types and the move, and keeps the relay compiling until
[Relay](/quest/m1/auth/relay.md) replaces its auth path.

- `moq_auth::Request` and `moq_auth::Grant` with the fields the questline
  lists, `serde` on both, `Event::{Connect, Revalidate, End}` as an enum,
  `Transport` re-using `moq_tokio::server::Transport`'s names, and the
  certificate facts as `moq_auth::Peer`. `Grant::validate` refuses a grant
  that names nothing, `revalidate` without `expires`, or an `expires` in the
  past, so a bad reply is refused once at the boundary. Document each field
  in one line, the way the questline says it out loud.
- `moq_auth::lease::{Producer, Consumer}`: `Producer::new(grant)` returns
  the pair; `Producer::update(grant)` and `Producer::revoke(reason)`;
  dropping the `Producer` revokes. `Consumer::grant()` reads the current
  grant, `Consumer::changed()` resolves on an update, `Consumer::closed()`
  resolves with the reason, and `Consumer::close(reason)` is the terminal
  call that consumes the handle with the session's own close
  classification; a bare drop is `Reason::Dropped`. No callbacks, no
  trait.
- `moq_auth::Client::new(url, tls)` and `Client::connect(request, bytes:
  Counters) -> Result<lease::Consumer>`: POSTs `connect`, validates the
  reply, builds the pair, and spawns the driver that re-POSTs `revalidate` on
  cadence with jittered backoff on failure until `expires`, applies each
  reply through `Producer::update`, and POSTs `end` with reason, duration,
  and bytes when the `Consumer` is closed or dropped, the reason being what
  `close` was given, `Dropped`, or the revocation the client itself
  issued. `moq_auth::Counters` is a cheap
  clone of two shared atomic totals the session adds to as it sends and
  receives, so the client never reaches into a session and a caller with no
  meter passes `Counters::default()`. `http://` is refused for a
  non-loopback host at construction, `https://` presents the given client
  identity, and `unix://` speaks HTTP over the socket.
- Move `rs/moq-token` in: `Claims { root, publish: Patterns, subscribe:
  Patterns, expires, issued }` serialized as `root`, `publish`, `subscribe`,
  `exp`, `iat`; the old `put` and `get` names are unknown fields and fail
  verification. `Key`, `Jwk`, `KeyId`, `Algorithm`, the key set, and
  `authorize` come over with their names, returning pattern residuals through
  `moq_pattern`. `js/token` becomes `js/auth` with the same zod shapes plus
  the request and grant schemas, so a Worker or a Node server validates a
  request and builds a grant with one import. The cross-language vectors
  in `js/token/src/interop.test.ts` move and gain a request and a grant.
- `moq auth generate|sign|verify` in `rs/moq-cli`, nesting the former
  `moq_token_cli::Args`; `--publish` and `--subscribe` take patterns, and the
  help says `foo/**` for a subtree. Delete `rs/moq-token-cli`, its workflow,
  Homebrew formula, nfpm packaging, and the `transition.yaml`, using the
  package-rename tooling in `rs/scripts` so the last `moq-token` release
  points at `moq-auth`. Retire the `@moq/token` npm package the same way.
- Consumers: `rs/moq-room` (`claims.rs`), `js/room` (`token.ts`), and
  `rs/moq-rtmp`'s references. `rs/moq-relay` keeps its flags and `auth.rs`
  in this quest and only reads pattern claims: a claim whose every pattern is
  prefix-shaped (a literal, `literal/**`, or bare `**`) maps onto its
  `PathPrefixes`,
  any other pattern is refused naming it. That adapter is deleted with the
  rest of `auth.rs` in the relay quest.
- Docs: `doc/lib/rs/moq-token.md` becomes `moq-auth.md` covering the
  request, grant, lease, client, and JWT; `doc/lib/js/token.md` becomes
  `auth.md`; `doc/lib/rs/index.md`, `doc/lib/js/index.md`,
  `doc/setup/install.md`, `doc/bin/cli.md`, the vitepress config, the root
  `README.md`, `infra/README.md`, `skills/moq/SKILL.md`, and every example
  invocation of `moq-token` or `moq token`. The Cross-Package Sync rows in
  the root `CLAUDE.md` that name `moq-token`, `js/token`, and
  `moq-token-cli` move to `moq-auth`, `js/auth`, and `moq auth`; this quest
  is the prompt for that edit.
- Tests: schema round trips in both languages, `Grant::validate` refusals,
  the lease pair through update, revoke, and drop, the client against a
  wiremock server for connect, revalidate cadence, refusal, outage until
  `expires`, and the end event with counters, and the moved JWT suite with
  the old `put`/`get` fixtures asserting refusal.

Public API: new `moq-auth` and `@moq/auth`; `moq-token`, `moq-token-cli`,
and `@moq/token` deleted; `moq token` becomes `moq auth`; claims change
shape. Wire: none.

## Related

- [Matcher](/quest/m2/path-patterns/matcher.md) - the pattern crate the
  claims and grants carry; `moq-pattern` and `@moq/pattern` already exist on
  dev (#3631), so this quest builds on them and Matcher closes when dev
  merges
