# [M] A v1 reply grants an mTLS peer its scope explicitly

## Goal

An endpoint that answers a v1 grant to an `mtls=<identity>` request decides
that peer's publish and subscribe scope in every mode, and an absent or empty
grant refuses it. `AuthToken::unrestricted` is minted only for an unversioned
reply, kept for endpoints that predate v1, and the docs say the door is open
there. A cluster peer dialing `/` is scoped like anyone else: its endpoint
grants everything explicitly. On dev.

## Plan

[Relay auth](/quest/m2/path-patterns/relay-auth.md) versions the auth API
grant shape: unversioned replies are v0 prefixes, v1 carries pattern grants.
This quest rides that v1 rather than inventing a version for mTLS.

- `authorize` scores an mTLS reply by version: v1 requires a grant and maps
  it onto the token's publish and subscribe patterns in both modes; v0 keeps
  today's alias and tier lookup and mints `unrestricted`, logging once per
  relay that mTLS peers are unrestricted because the endpoint is unversioned.
- The cluster keeps working on a v1 endpoint only when that endpoint grants
  the relay's own identity `[""]` for both; document that in
  `doc/bin/relay/cluster.md` beside the mTLS recommendation, and make the
  smoke cluster fixture's stub endpoint answer v1.
- `revalidate` stays `None` for mTLS peers, as the identity quest settles.
- Tests: a v1 empty grant refuses a peer in token mode; a v1 narrow grant
  scopes it; a v0 reply still admits it unrestricted with the one-line log;
  a cluster peer granted everything syncs. The mTLS section of
  `doc/bin/relay/auth.md` states the outcome per version.

## Required

- [mTLS identity](/quest/m2/auth-api/mtls-identity.md) - the request the
  grant answers
- [Relay auth](/quest/m2/path-patterns/relay-auth.md) - the versioned grant
  shape

## Closes

- [#3603](https://github.com/moq-dev/moq/issues/3603) - close this issue when the quest finishes
