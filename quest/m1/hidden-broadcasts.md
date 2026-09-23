# [L] Hidden broadcasts

## Goal

A broadcast whose path has a segment starting with `.` below the requested
prefix is left out of announce discovery unless the request opts in. A prefix
that names the dot segment itself (`.stats/`) lists what is under it, and
subscribing to an exact hidden path needs no opt-in. Clients that predate the
opt-in never discover hidden paths, so a platform can add `.`-named
broadcasts (stats, internal routes) without breaking deployed apps that list
everything and play what they find. moq.pro broke a robotics customer this
way.

Non-goals: access control (tokens still decide what a session reads), a
publisher-side flag (the name alone decides), and suffixes (`catalog.pro`
stays visible; only a leading `.` hides a segment).

## Plan

- Wire: lite-07 adds a hidden opt-in to ANNOUNCE_REQUEST. Every earlier lite
  version decodes as not opted in. IETF sessions get the same opt-in as a
  SUBSCRIBE_NAMESPACE parameter, absent meaning hidden. Specify both in
  `drafts/` and validate with `just drafts check`.
- Filter on the serving side, where `lite/publisher.rs` already scopes a
  request to its prefix and the token, and in the IETF publisher, so a hidden
  path never reaches a session that did not ask. The local origin consumer
  applies the same rule, so in-process and remote discovery agree.
- API, per call in both languages: JS `announced(scope, { hidden: true })`,
  and in Rust the equivalent on the scoped consumer. Rust sends one
  ANNOUNCE_REQUEST per literal head of the session's allowed patterns today,
  so settle how a per-scope opt-in reaches its own request.
- Cluster peers opt in, so `.internal/origins` and an embedder's dot paths
  still cross the mesh. For the rollout only, a relay treats an authenticated
  cluster peer that negotiated below lite-07 as opted in, so a mixed-version
  mesh keeps its dot paths. Delete that exemption in a follow-up once
  deployments run lite-07 everywhere.
- Docs: a hidden-broadcasts section in the announce docs
  (`doc/concept/moq-lite.md` or wherever announce is explained).
- Tests: the rule (dot segment below the prefix hidden, inside the prefix
  listed, exact subscribe works), the flag round trip per version, and a
  relay test where a lite-06 client listing the root misses `.x/y`, an
  opted-in lite-07 client sees it, and mixed-version peers keep
  `.internal/origins`.
- Hiding by default changes what existing clients see: decide at PR time
  whether that retargets to `dev` under the root `CLAUDE.md`.

## Related

- [IETF announce count](/quest/m1/ietf-announce-count.md) - another opt-in moq-transport extension on the namespace subscription
