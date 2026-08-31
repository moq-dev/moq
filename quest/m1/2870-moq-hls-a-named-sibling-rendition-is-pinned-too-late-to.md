# [M] moq-hls: a named sibling rendition is pinned too late to survive a same-path republish

## Goal

Implement and verify the behavior tracked in [#2870](https://github.com/moq-dev/moq/issues/2870)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

#### Problem

\#2795 bound renditions to the broadcast their catalog came from, so a same-path republish (whose group numbering restarts) cannot have its media served under the replaced broadcast's segment numbers, durations, and PROGRAM-DATE-TIME. It closed this for both rendition shapes:

- **inline** (`config.broadcast == None`, the catalog's own broadcast)  -  seeded at construction via `upstream.local(...)`.
- **named sibling** (`config.broadcast == Some(path)`)  -  resolved at the per-rendition watcher's start, *before* the watcher pushed its first timeline row, so the timeline and the media it describes always agreed.

On `dev` (after #2852 merges `main`), only the first half survives. `dev`'s HLS export is fetch-on-demand: there is one watcher per timeline *section* in `export/mod.rs` fanning entries out to every rendition, rather than main's watcher per *rendition*. The sibling resolution therefore has nowhere to hang, and `Rendition::track` resolves lazily on the first media request instead:

```rust
async fn track(&self) -> Option<moq_net::track::Consumer> {
    if self.live.broadcast.get().is_none() {
        let broadcast = self.source.resolve(self.sibling.as_ref()).await.ok()?;
        let _ = self.live.broadcast.set(broadcast);
    }
    ...
}
```

That leaves a window: the fanout can publish timeline rows for a sibling rendition while `live.broadcast` is still unset. A same-path republish landing in that window means the lazy `resolve` returns the *replacement* broadcast, whose restarted group numbers are then served under the old catalog's segment ranges, durations, and wall-clock metadata. This is exactly the failure #2795 describes, reachable only through the sibling path.

The pin itself is correct once taken (`OnceLock::set`, so the first resolution wins). The problem is purely when it is taken.

Note this is not a regression from the merge: `dev` always resolved lazily, and #2852 carries main's inline fix plus its regression test. It is main's sibling fix failing to carry across an architectural difference, so it should be closed deliberately rather than assumed handled.

#### Why the test doesn't catch it

`a_replacement_publisher_is_not_served_under_the_replaced_catalog` survived the merge and is not vacuous, but its rendition is built from a plain `VideoConfig::new(VideoCodec::VP8)` with no `broadcast` reference  -  an inline rendition. It exercises only the eagerly-seeded path.

#### Suggested direction

Resolve each rendition's sibling broadcast after construction but before the section watcher pushes any entry to it. Construction itself can't do it: `renditions::Producer::sync` runs synchronously under a write lock, which is why main used the watcher. Then add a regression that republishes a *named sibling* before the first media request and proves the replacement's bytes are not served under the old catalog.

Found by Codex adversarial review of #2852, verified against `main`'s `export/rendition.rs::watch` and `dev`'s fanout in `export/mod.rs`.

## Closes

- [#2870](https://github.com/moq-dev/moq/issues/2870) - close this issue when the quest finishes
