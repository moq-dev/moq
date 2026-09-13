# [S] Plan the auth API line

## Goal

The auth API questline is executable end to end: every quest in it names its
decisions, and the one question still open is answered. Run `/plan-quests`
on this file with what is already settled below; the output replaces this
quest with the updated line.

## Plan

Priority, stated by the maintainer on 2026-09-13: the cacheable path first,
because moq.pro's `/cluster/auth` serves a `kid` once per relay per cadence
however many viewers share it; the passthrough (proxy) mode stays for
operators who would rather answer a webhook, and must not cost the cacheable
path its property.

Settled on 2026-09-13, so the interview starts past them:

- The reply carries a top-level `"v": 1` to signal pattern grants; absent
  `v` means v0 prefixes. An unknown `v`, or v0 and v1 fields mixed, refuses
  the connection with its own error and never falls back.
- `mtls=<identity>` is the leaf's first SAN DNS name, then its CN, then its
  SHA-256 fingerprint, never empty.
- Under a v1 endpoint a cluster peer is scoped like any peer: the endpoint
  grants the relay's identity everything explicitly. moq.pro's Worker ships
  that reply before its relays upgrade.
- A re-check whose grant is narrower resizes the live session in place;
  the path-patterns relay-auth quest owns the resize and this line inherits it.
- mTLS peers are re-checked on the same `max-age` and `stale-if-error`
  semantics as tokens: a refusing endpoint drops the peer within two
  cadences, an outage rides the stale window. The questline README's "never
  revalidated" and the identity quest's `revalidate` stays `None` are
  superseded. Still open here: whether the relay floors the staleness window
  for mesh peers, which was the reason the old rule pinned `None`.
- Every request carries `root`, `transport`, and `host` in every mode, with
  `transport` required rather than optional; `mtls` when present.
- A `404` refuses at admission as it does on re-check; `401`/`403` refuse
  when a credential was forwarded and count as outages otherwise; `5xx`,
  network, and unknown errors are outages served from stale cache. The relay
  adds no request-rate cap; coalescing and HTTP caching are the throttles.
- Token and proxy stay two explicit modes.

Open, and the reason this quest exists: proxy mode is a per-relay flag that
caches per credential, while token mode never forwards the credential, so one
relay cannot today serve a JWT project on per-`kid` caching and a verdict
project at once. Candidates: an `auto` mode where a credential that parses as
a JWT with a `kid` takes the token path and anything else takes the proxy
path; a proxy-mode fleet whose endpoint verifies JWTs itself; or a separate
listener for verdict projects. Only `auto` adds relay complexity; the other
two are operator layout, so weigh it in the interview rather than assuming
it. The priority above favors whichever keeps per-`kid` caching for JWTs.
Decide it, then size and order the quests here,
including a docs quest: the reference in `doc/bin/relay/auth.md` has no single
table for the whole reply, `config.md` never shows `api_mode`, and the
admission-time status mapping is written nowhere.

## Related

- [Relay auth](/quest/m2/path-patterns/relay-auth.md) - owns the v1 grant
  shape and the resize
- [In-band auth](/quest/m2/auth/README.md) - the wire side of the same scope
