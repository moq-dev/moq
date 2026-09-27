# [M] Public rules parity

## Goal

Public and mTLS rules (`[auth] public*`, `--auth-public*`, and
`moq auth serve --public-*`/`--mtls-*`) are authorized like a JWT with root
`/`, as static public rules were in moq-relay 0.14.18. `public = "anon/**"`
(0.14 spelled it `public = "anon"`) lets an anonymous client dialed at `/anon`
publish `test.hang` (absolute `anon/test.hang`) and refuses a session dialed
outside `anon/`. Relay tests cover it.

This is a security fix for released 0.15: today an anonymous client can reach
into any path. It lands first and on its own, ahead of
[Auth parity](/quest/m0/auth-parity.md).

Non-goals: the pattern grammar (owned by [Path patterns](/quest/m1/path-patterns.md))
and JWT grant semantics, which already behave this way.

## Plan

### Model

A grant names what it may access from an absolute root. The session root is
the dialed path, and the patterns are rebased onto it by
`Claims::authorize`. For access to `demo/**`:

| Dialed | Session root | Patterns |
|---|---|---|
| `/demo/bar` | `demo/bar` | `**` |
| `/demo` | `demo` | `**` |
| `/` | `` | `demo/**` |
| `/other` | refused | |

JWTs already work this way. Public and mTLS rules must too, in the relay and
in `moq auth serve`. The Grant contract is unchanged: `root: None` still means
the dialed path.

### Cause

0.14 built static public access as `Claims::default().with_root("")` and ran
it through `Claims::authorize(path)`. Its public and auth APIs rooted their
answers at the dialed path, as `--auth-url` still does. #3688 (relay) and #3686 (`moq auth serve`)
replaced that with raw patterns in a `Grant` whose `root` is `None`, which
roots them at the dialed path. `anon/**` dialed at `/anon` therefore enforces
`anon/anon/**`. The `accepted` log was accurate: `root=event subscribe=event/**`
is root-relative and meant `event/event/**`.

The same cause admits every path. Under `anon/**`, a session dialed at
`/rooms/123` gets `rooms/123/anon/**`, so an anonymous client can publish
inside a JWT-protected room. A rule that starts with a wildcard is worse:
`public_subscribe = "*"`, meant as the top-level broadcasts, lets a session
dialed at `/rooms/123` subscribe to every broadcast in that room. 0.14
answered 401. It also breaks HTTP:
`/fetch/anon/bbb/video` scopes to `anon/bbb/anon/**` and times out with 504,
and `/announced/anon` lists nothing. The demo's meet page dials
`/anon/meet/<room>`, which fails under `prod.toml`'s suggested
`--public-publish 'anon/**'`.

### Decisions

- `Decider::Public` in `rs/moq-relay/src/auth.rs` and the public and mTLS
  branches of `serve::Policy::decide` in `rs/moq-auth/src/serve.rs` build
  `Claims { root: "", publish, subscribe, .. }` and call
  `authorize(&request.path)`. This adds no public API. `grant.root = Some("")`
  would be wrong, because it would move the session root to `/`.
- A dialed path the rules don't reach is refused, as 0.14 did
  (`ExpectedToken`) and as a JWT is (`RootMismatch` / `NoAccess`). It is a
  refusal (401 or 403), never an outage: the relay's
  `From<moq_auth::Error>` maps everything but `Refused` to a 502 that clients
  retry forever, so map `NoAccess` explicitly.
- A public or mTLS pattern with no wildcard (`anon`, `""`) refuses to start,
  naming `anon/**` for the subtree. This is a migration guard: 0.14 read
  `anon` as a prefix and 0.15 reads it as exact, so either silent reading
  misleads someone, and an operator fixes it before any client connects. It
  also catches an empty `MOQ_AUTH_PUBLIC=` from an unset variable, which
  today enables anonymous access. The cost is that no exact broadcast can be
  made public; the guard can relax once 0.14 configs are gone.
- The `accepted` log stays root-relative; the fix makes its output correct.
- `/fetch` keeps dialing the whole broadcast path, as 0.14 did. The rebase
  makes it work under public rules.

### Tests

Every existing integration test grants bare `**`, which reads the same
whether relative or absolute, so it hides the bug. Use rooted patterns such as
`anon/**` and `anon/*` throughout, which read differently at every dialed path
but the root.

- Unit: relay `Decider::Public` and `serve::Policy` public and mTLS rules with
  `anon/**`, dialed at `/`, `/anon`, `/anon/room`, and `/other`. Fix
  `a_public_config_admits_anonymous_and_certificate_alike`, which asserts the
  bug.
- Unit: a bare pattern refuses to start in both places.
- Unit: restore 0.14's public-subscribe-only and public-publish-only tests.
- Smoke: with `public = "anon/**"`, a client dialed at `/anon` publishes
  `test.hang`, a subscriber at `/` sees `anon/test.hang`, and a session at
  `/rooms/123` is refused. With `public_subscribe = "*"`, a session at
  `/rooms/123` is refused.
- Smoke: `/fetch` and `/announced` serve a broadcast under `anon/**` and
  refuse one outside it.
- Smoke: relay `--auth-url` to `moq auth serve --public-subscribe 'event/**'`,
  with an anonymous viewer dialed at `/event` watching `cam1.hang` published
  under a `root=event` JWT.

### Docs

- `doc/bin/relay/auth.md:13` says "patterns under the dialed path". State the
  model above once, and point the JWT and public sections at it.
- `doc/bin/relay/auth.md:286` lists `--auth-public` as removed; it is current.
  Check every row of that migration table against a test.
- `doc/bin/relay/index.md:41` gives `public = ""` for everything; use `"**"`.
  `index.md:15` still mentions anonymous prefixes and an auth API.
- `doc/setup/upgrade.md:65-68`: bare prefixes now refuse to start.
- `doc/bin/relay/http.md`: `/announced/<p>` returns names relative to `p`.
- `rs/moq-auth/src/serve.rs:3-4` and `claims.rs:64-68` claim public rules
  match 0.14 or share `Permissions`' frame; make them true.

## Related

- [Auth parity](/quest/m0/auth-parity.md) - the other auth behaviors 0.15 changed silently
- [Path patterns](/quest/m1/path-patterns.md) - owns the grammar these rules use
