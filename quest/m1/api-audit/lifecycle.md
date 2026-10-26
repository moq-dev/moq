# [L] One lifecycle vocabulary

## Goal

Every model handle in moq-net and `@moq/net` (broadcast, track, group, their
demand, `Finish`, listings) uses each lifecycle word with one meaning, and
Rust and JS agree on when each observer fires.

## Plan

Decided 2026-10-23 (API audit follow-up, maintainer), from a matrix of every
handle's writer verbs and observers:

- **`finish()` / `finish_at(end)`** declare a clean end. Not terminal: lower
  writes stay possible. JS gains `Track.Producer.finish()`.
- **`abort(err)`** ends with an error and is terminal. An abort after the
  outcome settled (a finished group, a track whose end was reached) only
  releases cache: readers keep the clean outcome. Tracks behave this way
  today; groups change to match and record no `cut` unless the abort is
  eviction policy.
- **`closed()`** on producers, `Finish`, and demand handles resolves once the
  outcome is final: a settled finish, an abort, or the last writer dropping.
  Rust returns `Result<(), Error>` (`Ok` for clean), JS `Error | null`. It
  never fires because readers left; `unused` reports that. Today Rust
  `closed()` ignores finish at every layer and reads `Dropped` after a clean
  close.
- **`close()`** is JS-only and releases that handle, like a Rust drop. It
  never changes state other handles share. Today a JS group, announce, or
  broadcast consumer's `close()` ends it for every reader, and the last
  broadcast consumer's close ends the producer; check what the JS wire layer
  relies on before changing it. `broadcast::Producer::close` keeps its name:
  a broadcast has no abort, and it means "drop every clone now".

Changes:

- Rust `track::Producer::close()`, crate-private since
  [moq-dev/moq#6747](https://github.com/moq-dev/moq/pull/6747),
  folds into `abort` with the clean-close error
  ([moq-dev/moq#6482](https://github.com/moq-dev/moq/pull/6482) makes it
  `Stream(Session(Cancel))`). Readers that drained a closed copy to a clean
  end now get that error: update `resume.rs`'s clean-end mapping and the
  sessions that call it; failover is unchanged since `route_failed` already
  accepts it.
- Rust `group::Producer::closed`/`poll_closed` fire on finish; sessions that
  need abort-only get a private poll.
- JS `Group.Producer.close(abort?)` and `Track.Producer.close(abort?)` split
  into `finish()` and `abort(err)`.
- JS `Finish.closed` also fires when a newer epoch supersedes it, as Rust does.
- JS `Lister.ended` becomes `closed`. JS `Demand.unused` rejects on close as
  Rust's does.
- Rust `track::Producer::end()` becomes `finished()` (the declared end, a
  snapshot). JS `Subscriber.end()` and `Ordered.end()` go: readers await
  `finished()`.

Behavior changes get tests on mock time in both languages.

Public API: moq-net and `@moq/net` breaking (observer semantics, JS producer
verbs, renames). Wire: none.
