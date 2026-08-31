# [XL] Dynamic announcements, broadcast epochs, and ended (VOD) broadcasts

## Goal

Implement and verify the behavior tracked in [#2610](https://github.com/moq-dev/moq/issues/2610)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

#### Problem

Dynamic broadcasts are broken for everyone except the JS client. Only `Connection.consume(path)` sends a blind SUBSCRIBE for a non-announced path, which is what triggers `origin.dynamic()` on the relay. The Rust client is origin-mediated: its subscriber half only materializes broadcasts from announces, and `request_broadcast()` on the client's local origin has no handler, so it resolves to `Unroutable` immediately. `origin::Dynamic`'s own docs describe the session as "a fallback router, fetching a broadcast from upstream on demand", but no session ever registers one.

Two more gaps compound it:

- There is no way to answer announce interest dynamically. `recv_announce` streams `origin.announced()` (the static tree) and never surfaces the requested prefix to the application, so a VOD library or transcoder cannot advertise content on demand.
- Broadcast identity is a hack. The first hop of the route chain stands in for content identity, so a restarting publisher with a stable origin id gets falsely spliced, a restarted process cannot resume content it legitimately could, and "newest announcement wins" races a stale generation arriving over a fresh connection.

#### Design

Spec'd in the moq-lite-06 WIP section of `drafts/draft-lcurley-moq-lite.md` (unpublished, so amended in place) plus a new `drafts/draft-lcurley-moq-broadcast.md` for the IETF side (EPOCH as a negotiated moq-transport parameter, mirroring relay-hops). Draft PR to follow from `claude/dynamic-broadcasts-api-97230b`.

Wire summary (lite-06):

- `ANNOUNCE_START` / `ANNOUNCE_UPDATE` (renamed from `ANNOUNCE_RESTART`) gain `Epoch (i)` and `Ended (i)`.
- Epoch is the per-path content generation: publisher-minted, strictly increasing, forwarded unchanged, 0 = unspecified. Highest wins (non-zero outranks 0), replacement is decided by value rather than arrival order, equal non-zero epochs splice, first-entry identity survives only for zero-vs-zero. Wall clock is a seed, not a guarantee; receivers keep no high-water mark, which bounds the damage of a bad epoch to its advertisement's lifetime.
- `Ended` splits live from complete/static content: ended broadcasts reject SUBSCRIBE, are read via FETCH, and are only announced on streams whose `ANNOUNCE_REQUEST` opted in via its new `Ended (i)` filter (the VOD enumeration path; moq.pro can store recordings at `<broadcast>/<epoch>` instead of UUID + API).
- `SUBSCRIBE`, `FETCH`, and `TRACK` gain `Epoch (i)` (0 = current, mismatch = reset); `TRACK_INFO` returns the resolved epoch so metadata caches key by generation and requests cannot race a replacement.

#### Implementation plan

##### 1. Model: epoch replaces first-hop identity (`rs/moq-net`)

- \[ ] Add `epoch` and `ended` to `broadcast::Route`; mint helper (max of wall clock and observed incumbent + 1).
- \[ ] Replace `FrontState.publisher` and the `restart_announce` first-hop comparison with the epoch rules (equal non-zero = attach/splice, higher = takeover, lower = ignore, zero-vs-zero = existing first-hop behavior).
- \[ ] Key the linger re-attach check by epoch.
- \[ ] Reject SUBSCRIBE and gate announces for ended broadcasts; announce visibility of ended entries only to consumers that opted in.

##### 2. Wire: lite-06 codecs and loops (`rs/moq-net`, `js/net`)

- \[ ] Message structs and per-version decode arms for the new fields; rename `AnnounceRestart` to `AnnounceUpdate` (and the JS mirrors).
- \[ ] Publisher/subscriber announce loops carry epoch/ended through; subscribe/fetch/track paths enforce epoch match and return resolved epoch in TRACK\_INFO.
- \[ ] JS `Connection.consume` and friends expose epoch pinning and ended filtering.

##### 3. Origin API: dynamic announce interest (`rs/moq-net`)

- \[ ] New handle alongside `origin.dynamic()` (e.g. `requested_announce()`): yields `{prefix, ended}` interest events, refcounted per distinct prefix, delivered to every handler, released when the last matching announce stream closes. Handlers respond by creating real (possibly ended) broadcasts through a scoped producer.
- \[ ] `recv_announce` registers interest for the requested prefix; `Consumer::announced()` raises it locally too so in-process consumers behave like wire peers.
- \[ ] Keep `origin.dynamic()` as the exact-path fallback, unchanged.

##### 4. Session wiring: Rust clients reach dynamic origins

- \[ ] Opt-in: the subscriber half registers an `origin::Dynamic` handler and serves misses by wire-subscribing (create the broadcast with `announce: false` so it stays out of the tree's announce set but dedups repeat requests). This alone fixes the headline bug.
- \[ ] Follow-up (separate PR): forward locally raised interest upstream as narrower ANNOUNCE\_REQUESTs, deduped against covering prefixes, so `announced_broadcast()` works end-to-end across hops.

##### 5. IETF adapter and bindings

- \[ ] `rs/moq-net` ietf module: EPOCH parameter per `draft-lcurley-moq-broadcast` (negotiated setup option, strip when not negotiated).
- \[ ] Cross-package sync per the table: `moq-ffi` + libmoq + py/swift/kt/go wrappers, `js/net`, `doc/concept`, relay docs.
- \[ ] `just test smoke-full` for the interop matrix.

##### Tests to encode the root causes

- \[ ] Stale generation arriving over a fresh connection must not displace the current one (the arrival-order race).
- \[ ] SUBSCRIBE/FETCH/TRACK crossing an in-flight replacement resets instead of serving mixed generations.
- \[ ] Rust client `request_broadcast` against a relay `origin.dynamic()` handler round-trips.
- \[ ] Ended broadcast: SUBSCRIBE rejected, FETCH served, announced only to opted-in streams.

Assumption worth confirming: moq-lite-06 has not shipped in any deployed relay/client, so its wire format can change in place. If anything already speaks the current lite-06 framing, these fields need a lite-07 instead.

## Closes

- [#2610](https://github.com/moq-dev/moq/issues/2610) - close this issue when the quest finishes
