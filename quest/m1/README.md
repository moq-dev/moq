# m1: the dev line

## Goal

Work intended to land on `dev` before it merges to `main`, or be explicitly
deferred by the maintainer: the breaking
API and wire changes (the announce and wildcard surface, error codes, the
allocator mirrors, the bindings), the merge gates, and the merge itself.

## Plan

Branch a quest from `dev` when it breaks a published API or wire. A quest
stays here only if it breaks a published API or wire, or gates the merge;
[Merge dev](/quest/m1/merge-dev.md) names the gates, and [Release](/quest/m1/release.md)
names what gates the release after it. Work that is identical
on `main`, additive, or targets a `0.0.x` crate lives in
[m2](/quest/m2/README.md) even when it builds on dev-only code; it starts on
`main` after the merge. The 2026-09-12 grooming applied that rule to every
quest here and merged main into dev. The auth API line is here for its
request-side break (`mtls=<identity>` and the now-required fields) and ranks
first because moq.pro adopts the release only once that contract is settled;
it is priority, not a merge gate, and [Merge dev](/quest/m1/merge-dev.md)
does not require it. [Session revalidate](/quest/m1/session-revalidate.md)
is on the line for the same reason: the lease's push surface is part of that
contract, so it gates [Release](/quest/m1/release.md) rather than the merge.

## Quests

- [One auth path](/quest/m1/auth-one-path.md) - the server and public modes become clients of the same `Admissions` queue the embedder answers, a reshape of the unpublished auth API that is dev-line work by priority, not a gate
- [Session revalidate](/quest/m1/session-revalidate.md) - the internal listener re-checks sessions by id, path, or address now, so a kick lands in one round trip and the cadence stops carrying it; lands with the auth API and gates the release, not the merge
- [Auth contract](/quest/m1/auth-contract.md) - moq-auth and @moq/auth ship one type per concept: no `Counters`, one publish/subscribe pair, a reference server that scopes like the library, no `kty` default
- [Announce event](/quest/m1/api-net-announce.md) - publishers announce prefixes on every wire, consumers scoped by a pattern read the covered path already trimmed, with no `as_prefix().expect()` at 89 call sites
- [Origin scoping](/quest/m1/api-net-origin.md) - `scope(root, patterns)` is one fallible call, a fresh origin has a random hop, and the handles stop derefing to `Hop`
- [Bindings announce match](/quest/m1/api-origin-scopes.md) - every binding takes a pattern scope and reports the announce match with its captures
- [PathPrefixes](/quest/m1/api-path-prefixes.md) - the unused moq_net::PathPrefixes type is deleted before the release
- [moq-tokio shapes](/quest/m1/api-tokio-shapes.md) - a `Drop` on `Listener`, a worker `Member` that cannot be cross-wired, `std::time::Duration` fields, one construction idiom, no six-argument merge
- [@moq/net API](/quest/m1/api-js-net.md) - one error namespace with Rust's names, one connect shape, one path-to-broadcast call, `Time.Milli` everywhere, wire-layer methods internal
- [@moq/auth API](/quest/m1/api-js-auth.md) - `Key` and `KeySet` namespaces with moq-auth's verbs instead of `load`/`signWith`
- [JSON configs](/quest/m1/api-json-config.md) - window takes the Config shape snapshot and stream have, mux takes the hang entry, the dead per-track `timeline` field goes
- [Catalog types](/quest/m1/api-hang-catalog.md) - `hang::Catalog<E>` is the one section list, `Clock` holds a `Timestamp`, `Timeline` folds into `Archive`
- [Rendition ownership](/quest/m1/api-mux-rendition.md) - one handle publishes a media track and reports its estimate, instead of five
- [Watch and publish shapes](/quest/m1/api-watch-publish.md) - props objects everywhere, silent `latency`/`jitter` aliases refuse, `Sync` stops needing a jitter bridge, rooms get bandwidth
- [Gateway types](/quest/m1/api-gateways.md) - no `anyhow` in a gateway `Error`, `PathOwned` prefixes, `Duration` segments, `moq_rtc::Server::new(config)`, an SRT reject with a reason
- [libmoq units](/quest/m1/api-libmoq-units.md) - `moq_client_config` is all microseconds, the header declares every enum and error code, NULL callbacks are refused
- [Cluster -01](/quest/m1/cluster-01/README.md) - rs/moq-net and js/net speak the revised cluster extension (HOP_ID, REQUEST_UPDATE repricing) and -01 is published
- [API review gate](/quest/m1/api-review-gate.md) - each `api-*` quest above is landed or deferred by the maintainer before the merge PR opens
- [Merge dev](/quest/m1/merge-dev.md) - dev lands on main with a closing keyword for every issue it fixed
- [Release](/quest/m1/release.md) - the release moq.pro adopts: binding parity, an upgrade page, and a staging soak gate it rather than the merge
