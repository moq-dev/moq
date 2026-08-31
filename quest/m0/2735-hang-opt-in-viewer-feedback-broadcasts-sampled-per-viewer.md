# [M] hang: opt-in viewer feedback broadcasts (sampled per-viewer QoS)

## Goal

Implement and verify the behavior tracked in [#2735](https://github.com/moq-dev/moq/issues/2735)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

### hang: opt-in viewer feedback broadcasts (sampled per-viewer QoS)

Design context: moq-dev/moq.pro#992. **Deliberately the last phase** - relay-side aggregation of per-subscription backlog covers congestion for 100% of viewers with zero client cooperation. What it can't cover is the per-viewer *experience* distribution: startup time, rebuffer ratio, e2e latency percentiles, decode/render stalls on a healthy network, device context. That's this issue - the mux-data half. Filed now so the design isn't lost.

#### What this is not

Not a congestion-control channel. Bandwidth adaptation in MoQ is per-hop by design (PROBE); there is no end-to-end REMB equivalent to build. And PLI is mostly obviated by group-per-keyframe + `group_start`/`fetch_group` - a viewer recovers at the next group boundary. Feedback carries *what happened*, low-rate, which is what makes sampling acceptable.

#### Sketch

- **Path**: a dotted sibling namespace rather than a child of the watched broadcast - e.g. `.feedback/<broadcast-path>/<viewer-id>` relative to the auth root. A child path (`room/name.hang/<rand>`) sits inside the publisher's prefix: anyone with `get` on the room reads every viewer's feedback, and viewer churn spams announces into the prefix everyone watches. A dotted namespace separates the ACL the way `.stats` already does.
- **Opt-in is a token decision**: mint viewer tokens with `put: [".feedback/<path>/<viewer-id>"]` for a sampled fraction of viewers, and simply don't for the rest - handles both "optional" and "big broadcasts don't do this". The viewer id lives in the token so viewers can't clobber or impersonate each other. `Claims`/`Scope` express this today; the signing key's immutable `Scope` must cover the feedback prefix in the publish role (one prefix under the root, so scoped keys express it naturally).
- **Discovery**: the catalog advertises it (`{ "feedback": { "prefix": "../.feedback/<path>" } }`) so players know to bother; also where sampling parameters could live if the publisher rather than the token should control rate.
- **Content**: one small `feedback.json` snapshot track, ~1 update/5s. Everything to report already exists in `js/watch`, computed and discarded: `Sync.received()`'s `now - timestamp` (latency proxy; true e2e once publishers set `Timeline.wall`), the per-label late-frame tracking in `Sync.#late` (currently flushed to console.debug), `DecoderOutput.stalled` / audio ring-buffer stalls (booleans, integrable into rebuffer time), group skips decided in `container/consumer.ts` (counted nowhere), `connection.stats()` + the viewer's own PROBE for the last hop.
- **Aggregation** is the consumer's problem (for the CDN: the edge slurps the prefix and folds distributions; per-viewer drill-down subscribes an individual feedback broadcast directly).

#### Security considerations for the draft

Feedback is a cardinality/amplification surface: relays MAY cap announced broadcasts per feedback prefix; tokens bound the writer set; snapshots are tiny and rate-limited by convention (and enforceable by the relay's usual means).

## Closes

- [#2735](https://github.com/moq-dev/moq/issues/2735) - close this issue when the quest finishes
