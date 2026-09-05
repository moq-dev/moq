# [S] moq-transcode: retirement never covers an in-flight fetch

## Goal

A test that fails against a reverted fix on every run and on any machine for
a rung retired while the fetch handler is still opening its decoder for a
group it has not yet claimed.

## Plan

The local coverage is now measured; the historical CI failure remains unexplained.
`retirement_finishes_an_in_flight_fetch` in `rs/moq-transcode/src/lib.rs` does
not reach the fetch handler in the measured local runs. Those runs exercise
a different property.

Established by instrumenting `rung::serve` (probe prints on the retire branch,
on `GroupRequest::accept`, and on the live session) and running the test 40
times:

- A `track::Consumer` is demand on its own (`Demand::used` is kio's "at least
  one consumer"), so `consumer.track("video/120p")` starts the live path
  immediately, before the test's `rung.info().await` even resolves.
- By the time the test calls `fetch_group(0, None)`, `rung.latest()` is already
  `Some(0)`: the live path created output group 0. `Consumer::fetch_group`
  resolves straight from the cache and queues no attempt, so `fetches` never
  pops a `GroupRequest` and never spawns a fetch task. 40/40 runs.
- What the test therefore covers is the live path riding out its open group
  after retirement. Reverting that ride-out (retire aborts `current` and breaks
  the session instead of continuing) fails the test 30/30. Reverting `serve` to
  a `tokio::select!` over the two halves fails it too, with `Err(Dropped)`,
  because the live arm is cancelled mid-group.
- Reverting the actual historical bug (`producer.finish()` back inside `live`
  at retirement, `serve` no longer finishing) leaves the whole `moq-transcode`
  suite green, confirming the failed negative control recorded in #3381.

The existing test cannot retire the rung while its fetch is opening a decoder:
it awaits `rung.fetch_group(0, None)` before changing the source catalog.
`fetch` opens `rung.pipeline()` before `GroupRequest::accept` inserts the output
group, and `Fetching::poll` returns that group only once it is cached. If the
live path inserts group 0 first, the fetch handler's later acceptance instead
returns `Duplicate`. Neither ordering establishes the previously claimed CI
mechanism. Recover the failing revision and trace before attributing that
failure to a specific interleaving.

Two comments that got this wrong on main were corrected without closing the
quest: the test's doc comment (which claimed `fetch_group` resolves before
`accept`) and `rung::serve`'s note on what `finish` takes as its boundary. A
third, in the retirement drain in `fetches`, claimed the track may already have
declared its final sequence; nothing can, since `serve` finishes only after
`tokio::join!` returns.

What is left is a test that reaches the fetch handler by construction and
triggers retirement before acceptance, without awaiting the output group first.
The obstacle is structural: holding the consumer needed to fetch is itself the
demand that starts the live path, and the live path serves the same group from
the same source, so any construction has to give the fetch a group the live
path cannot produce (a source group the shared feed does not deliver, served
through a `Dynamic` on the source track so it stays open while the fetch runs).
Verify any candidate against a revert that puts `finish` back in `live`, since
that is the revert the earlier attempts passed. Per no internal callbacks,
whatever makes this reachable is not a test hook parameter on `serve`.

Also open: the test's name says "in-flight fetch" for something that usually
exercises the live path. Renaming it was deliberately left to whoever makes the
fetch case real, rather than churning the name twice.

## Related

- [Congestion-aware transcode ladders](/quest/m1/ladder/README.md) - the rung lifecycle this boundary belongs to
