# [M] js/net: path-keyed publisher state goes stale when a broadcast is replaced

## Goal

Implement and verify the behavior tracked in [#2985](https://github.com/moq-dev/moq/issues/2985)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

#### Problem

Three pieces of publisher state in `js/net` are keyed by **path** and never reconciled when a path's producer is *replaced*. Replacement keeps the key present, so every one of them silently keeps serving the predecessor's generation.

Surfaced while reviewing #2976, which fixes an adjacent race (a same-tick close + republish let the predecessor's close handler evict the live successor from the lookup). That fix is correct and these are **not** regressions from it: the diff there only changes the body of `broadcast.closed.then(...)`, so it is inert unless a broadcast closes. All three reproduce on `main` today with a plain replacement over a still-open predecessor, since `Publisher.publish` overwrites the map entry without closing its predecessor:

```ts
publisher.publish(path, first);
publisher.publish(path, second); // first never closed; key unchanged, value swapped
```

##### 1. Same-path replacement is invisible to discovery

`runAnnounce` builds its active set from `broadcasts.keys()` and diffs it with `Set.difference`, so a value swap under an existing key produces neither an `ended` nor a new `active` announcement. Discovery consumers stay attached to the predecessor's `Consumer`.

- `js/net/src/lite/publisher.ts`  -  `runAnnounce`
- the IETF advertisement loops reconcile the same way

##### 2. `TRACK_INFO` is cached across generations

`#trackInfo` is a `Map<string, Promise<TrackInfoMessage>>` keyed by path + track name and is never invalidated by `publish`. A successor with different immutable track properties (timescale, ordering) is answered with the predecessor's metadata. `FETCH` consumes the same cache, so successor frames can be interpreted against predecessor metadata.

- `js/net/src/lite/publisher.ts`  -  `#trackInfo`, `runTrackInfo`

##### 3. An IETF refusal permanently suppresses the successor

The advertisement loop holds `refused` keyed by `Path.Valid` and clears an entry only when the path is absent from the live set (`if (!live.has(path)) refused.delete(path)`). Because replacement keeps the key present, a `PUBLISH_NAMESPACE` refusal against the predecessor is never cleared, and a live successor stays unadvertised for the rest of the session even though a direct `SUBSCRIBE` resolves it.

- `js/net/src/ietf/publisher.ts`  -  the advertisement loop's `refused` map

#### Suggested direction

All three are the same root cause: no producer identity or generation is tracked, only the path. #2610 specs exactly this as the broadcast **epoch**, and already lists TRACK\_INFO epoch-keying ("`TRACK_INFO` returns the resolved epoch so metadata caches key by generation and requests cannot race a replacement"). This issue tracks the three concrete `js/net` sites that are reachable today, so they don't get lost if the epoch work lands wire-first or in stages.

A local fix is possible ahead of the wire work: compare producer identity (not just the key) during announce reconciliation and emit the replacement transition, evict the path's `#trackInfo` entries in `publish`, and key `refused` by producer identity so it clears on replacement.

#### Tests to encode the root cause

- Replacing a path's producer (both same-tick-after-close and over a still-open predecessor) emits a replacement transition and `announcedBroadcast.active` becomes a distinct `Consumer`.
- Republishing with a different timescale returns the successor's `TRACK_INFO`, not the predecessor's, over both `SUBSCRIBE` and `FETCH`.
- A `PUBLISH_NAMESPACE` refusal followed by replacement advertises the successor.

Found by Codex during the adversarial review on #2976, verified against the code.

## Closes

- [#2985](https://github.com/moq-dev/moq/issues/2985) - close this issue when the quest finishes
