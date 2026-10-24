# [S] Tiers are metric-safe names

## Goal

A stats tier from an auth server cannot break or forge `/metrics` series. Every
tier the relay accepts is a slash-separated path of `[A-Za-z0-9._-]` segments,
and the relay escapes every label value it renders.

## Plan

Decided 2026-10-22 from the security audit, with the maintainer. `Grant.tier`
and `Refusal.tier` are unvalidated (`rs/moq-auth/src/grant.rs`,
`rs/moq-auth/src/refusal.rs`). The relay turns them into a `Tier`
(`rs/moq-relay/src/auth.rs`, `rs/moq-relay/src/refusals.rs`), and
`render_metrics` (`rs/moq-relay/src/internal.rs`) interpolates the tier raw
into `tier="..."`. An auth server that derives the tier from a token claim
can therefore inject `"`, `\`, or a newline. Release has the same
interpolation, but this is not backported: only an auth server sets the tier.

- ✅ Both validate and escape. Validation fails loud at the source;
  escaping keeps exposition correct for any label value, now or later.
- ✅ The rule: tier segments of ASCII letters, digits, `.`, `_`, and `-`,
  joined by `/`. Enforce it in `Grant::validate`, when a `Refusal` is
  parsed, in `moq-auth serve --tier`, and in the relay's tier configuration,
  so a bad tier is refused where it is set. Mirror the check in `js/auth`'s
  contract schemas.
- Escape `\`, `"`, and newline in every label value `render_metrics` and
  `rs/moq-relay/src/stun.rs` write, via one helper.

Tests: a grant and a refusal with a quote, backslash, or newline in the tier
are refused; a rendered label containing each character is escaped per the
Prometheus text format.

Docs: `doc/bin/relay/auth.md` states the tier rule. Public API: `Grant` and
`Refusal` refuse tiers they accepted before, so add a `MIGRATE.md` entry for
moq-auth and @moq/auth. Wire: the auth server's JSON accepts fewer tiers.

## Related

- [Relay metrics through prometheus-client](/quest/m2/relay-prometheus-client.md) - replaces hand-written exposition later; escaping lands here first
