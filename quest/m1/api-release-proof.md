# [L] Prove the dev API through packaged external consumers

## Goal

Before publishing the dev API, representative external browser and native
callers build and exercise the intended contracts, and every finding in the
2026-09-12 audit has an explicit fix or deferral decision.

## Plan

The audit inspected dev `e2350b39a6ce9bd0734841fc4b4ce399ee195562`, fetched
from origin on 2026-09-12, against main and the actual consumer at
`/home/kixelated/work/moq.pro`. Consumer code was read without modification;
it targets an older API. Compile migrations alone are not upstream defects.

Coverage included Rust origin/announce, sessions, track subscription/control,
group/frame ownership, timestamp invariants, bandwidth handles, JSON/binary
publishers, mux/hang boundaries, browser net/signals and JSON/binary wrappers,
watch/publish integration, shared FFI with C and native language wrappers,
and relay embedding. This is a source/API audit, not an exhaustive behavioral
proof of every exported symbol, platform backend, codec, or protocol version.

New findings and recommendations, ordered by consequence:

| Finding | Evidence status | Quest |
|---|---|---|
| FFI pending reads serialize independent datagram/group lanes | Source-traced starvation | [Read lanes](/quest/m1/api-ffi-read-lanes.md) |
| Shared/private connections disagree on credential-refresh recovery and terminal state | Reproduced; fixed: one URL-recovery contract, `closed` is handle disposal | completed |
| Relay embedding can discard newly added socket owners without a compile error | Source-traced; real edge embedding pattern | [Embedding](/quest/m1/api-relay-embedding.md) |
| Accepted numeric properties can truncate or fail only at encoding/receiving | JS unsafe timescales reproduced; remaining paths source-traced | [Numeric invariants](/quest/m1/api-numeric-invariants.md) |
| FFI first-frame convenience treats empty groups as EOF and loses an acquired group on cancellation | Source-traced | [Frame cursor](/quest/m1/api-ffi-frame-cursor.md) |
| JSON/binary readers hide the subscription cleanup handle | Abandoned: finish must be `&mut` so abort can follow | deferred |
| FFI configuration setters silently succeed without applying a value | Source-traced busy/closed branch | [Configuration](/quest/m1/api-ffi-configuration.md) |
| Local inclusive ends cannot express the empty exclusive range | Existing C adapter documents the mismatch | [Bounds](/quest/m1/api-subscription-bounds.md) |
| Typed Getter input can be rejected solely for lacking an internal brand | Fixed: getter() reuses any conforming Getter | Fixed |
| JSON edit guard logs failed implicit publication | Source-traced error suppression | [Edit transaction](/quest/m1/api-json-edit-commit.md) |
| Wrapper constructors diverge and consumers accept ignored producer knobs | Signature/implementation comparison | [Wrapper config](/quest/m1/api-js-wrapper-config.md) |
| Terminal publisher methods inconsistently retain the caller's handle | Signature comparison; maintainability recommendation | [Finish ownership](/quest/m1/api-producer-finish.md) |

Recommend resolving behavioral failures and published contract choices before
merge. Cosmetic consistency can be deferred explicitly if the maintainer
accepts the later breaking change. The table is a disposition checklist, not
an automatic declaration that every proposed quest must be implemented first.
Record each disposition and its fixing revision in the merge proof.

Build a small public fixture against packaged exports, outside workspace
hoisting and path aliases, modelling:

- Connection, announcements, credential refresh, and publication replacement.
  moq.pro `app/src/lib/live.svelte.ts:75,113` is the reference use case.
- Stats snapshots versus retained rollup groups; choose the intended mode
  instead of mechanically replacing a removed read helper.
- Catalog-only reading with full snapshots followed by deltas. The old
  consumer `app/src/lib/broadcastCatalog.svelte.ts:46` assumes every frame is
  a full catalog. Use Json.Snapshot.Consumer and the catalog schema as the
  upstream watch path does (`js/watch/src/broadcast.ts:304`); generic JSON
  reads plus schema validation do not reconstruct merge patches.
- Embedded relay with custom routes and application workers, based on
  moq.pro `rs/edge/src/main.rs:88,317`, through the settled owning API.

Do not copy the private application into the public repo. Include dependency
deduplication in packaged validation: moq.pro already tests physical package
identity (`app/test/deps.test.ts`); the module-local Connection pool and private
net hooks deserve a real two-copy test. Duplicate-copy failure remains an
unverified hypothesis until reproduced, not an established audit defect.

Update migration docs beside the affected APIs, including Connection exports,
broadcast.track(...).subscribe(...), Ordered readers, JSON modes, and the
moq_native to moq_tokio transition. Do not reintroduce obsolete wrappers to
avoid migration. Run the fixture in CI, record package versions and exact
revisions, and let moq.pro's separately owned pin/release process adopt the
proved surface. This quest does not bump packages or deploy the consumer.

Public API: no additional API change beyond the fixing quests; fixture and
documentation prove the chosen surface. Wire: no new format. Use existing
browser/native integration recipes and cross-language smoke as appropriate.

## Related

- [Merge dev](/quest/m1/merge-dev.md) - records the final release and interop proof
- [C ABI parity](/quest/m2/2152-libmoq-c-abi-catch-up-with-the-moq-ffi-surface.md) - already owns C request/server omissions
- [C fetch](https://github.com/moq-dev/moq/blob/e2350b39a6ce9bd0734841fc4b4ce399ee195562/quest/m2/libmoq-fetch.md) - already owns the missing C fetch entry point
