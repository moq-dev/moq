# [L] JWT-free verdict mode

## Goal

moq-relay can hand an opaque credential to its auth API and be told the grant,
instead of resolving a `kid` and verifying a JWT locally. Ships as
`--auth-api-mode proxy`.

## Plan

[#3044](https://github.com/moq-dev/moq/pull/3044) is the implementation and
targets dev. Landing it completes this quest; delete the quest in that PR. It
settled the design points this quest used to hold open, and they are recorded
here so review does not relitigate them:

- A mode, not a response shape. Letting one endpoint answer with a `key` for
  one connection and a `grant` for another put both paths inside a single
  request: which cache key applies, whether the credential may be sent, what
  "still vouched for" means. Choosing once per relay deletes all of it. The
  mode lives on `AuthApi`, so it rides on a session's grant and a proxy-admitted
  session re-checked by a token-mode instance keeps its grant.
- The credential travels as `Authorization: Bearer` on the existing GET. The
  relay caches on a SHA-256 of it and declares the cache private, so an endpoint
  that forgets `Vary: Authorization` cannot cross-serve grants and the secret
  stays out of logs and metrics.
- Refusal is `404`, an empty grant, or in proxy mode a `401`/`403`. Token mode
  and anonymous proxy connections carry no credential, so there those statuses
  stay an outage rather than disconnecting an audience over a gateway blip.
- Each re-check's `exp` replaces the last. In token mode the JWT's own `exp` is
  a ceiling a reply may lower but never raise.
- `--auth-api-mode proxy` excludes `--auth-domain`, and a mode without
  `--auth-api` is a startup error.
- `doc/bin/relay/auth.md` carries the mode, refusal statuses, expiry, cache
  semantics, and the cost trade: a credential shared across an audience caches
  like a `kid`, a per-viewer credential costs a request per viewer.

What the PR leaves out on purpose is the mTLS path, which still bypasses the
mode; that is [#3087](/quest/m2/3087-relay-mtls-peers-bypass-auth-api-mode-so-proxy-grants.md).
