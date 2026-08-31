# [M] origin: announce-only entries, advertise on-demand content without instantiating broadcasts

## Goal

Implement and verify the behavior tracked in [#2756](https://github.com/moq-dev/moq/issues/2756)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

#### Problem

Announcing a path requires instantiating a broadcast. `origin.publish` / `create_broadcast` put a live `broadcast::Producer` behind every announced entry, while `origin.dynamic()` broadcasts are deliberately never announced (#1772). So "announced" implies "instantiated", and there is no way to advertise content we could create on demand without actually starting to create it.

The concrete victim is any large on-demand catalog. A VOD library or transcoder that can serve 100k recordings must either instantiate 100k broadcast producers just to appear in announce streams, or stay invisible and force consumers to know paths out of band. #2610's dynamic announce interest (`requested_announce()`) surfaces the demand to the application, but its plan still has handlers respond by creating real broadcasts, which only moves the instantiation cost from attach time to interest time. Interest in a prefix should cost an enumeration, not a fleet of broadcasts.

#### Design

An **announce-only entry**: a path in the origin's announce tree with no front behind it.

- It shows up in every announce consumer and on the wire like any other announcement. The wire genuinely cannot tell: announcements never carried broadcast state, and the epoch/ended fields planned in #2610 attach naturally (`ended: true` is the VOD enumeration case). No wire change.
- A subscribe or request against it falls through to the existing dynamic/request machinery (`origin.dynamic()`, `request_broadcast`), which materializes the real broadcast on first demand. The materialized broadcast attaches at the same path and epoch and supersedes the placeholder; when it closes, the announce-only entry remains until its own handle is dropped.

Two API flavors that compose:

1. **Standing**: `Producer::announce(path, ...) -> guard` (JS: `origin.announce(path)`). Advertise regardless of interest; drop the guard to retract. For small known catalogs.
2. **Interest-scoped**: the `requested_announce()` handle from #2610 gains an `announce(suffix, ...)` response method. Entries live only while the interest that solicited them does: the application enumerates its catalog when some peer opens an announce stream for the prefix, and the whole subtree of announce state evaporates when the last matching stream closes.

Together with #2708 (lazy, prefix-scoped announce solicitation) this closes a fully demand-driven loop: interest flows upstream hop by hop as narrow ANNOUNCE\_REQUESTs, the application at the origin answers with cheap announce-only entries, and broadcasts only materialize when someone subscribes.

#### Notes

- The interest ledger proposed on #2708 is the shared foundation; flavor 2 depends on it and on #2610's `requested_announce()`. Flavor 1 is independent and could land first.
- Model hazard to design around: announce consumers hand out `broadcast::Consumer` fronts today. An announce-only entry has none, so either the announce event carries a lazily-materializing front (resolving through the request machinery on first track subscribe), or announce events decouple from fronts. The former keeps the consumer API unchanged and is the likely shape.
- Cross-package sync per the table: `js/net`, `moq-ffi` and wrappers if the surface reaches them, `doc/concept`, and `drafts/draft-lcurley-moq-lite.md` only if any wire clarification falls out (none expected).

Related: #2610 (dynamic announce interest, epochs, ended broadcasts), #2708 (lazy announce solicitation), #2412 (moq-gst publish-on-demand), #1772 (dynamic broadcasts are never announced).

## Closes

- [#2756](https://github.com/moq-dev/moq/issues/2756) - close this issue when the quest finishes
