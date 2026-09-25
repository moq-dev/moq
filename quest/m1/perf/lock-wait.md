# [S] Lock wait accounting

## Goal

Each io_uring worker reports how long it spent blocked on cross-worker
`kio::Lock`s, so the cost of the shared moq-net model on a thread-per-core
worker is a number rather than a suspicion. A worker blocked on a mutex has
SQEs staged and unsubmitted; the number decides whether that is worth
addressing.

## Plan

`kio::Lock` is a plain `std::sync::Mutex` (rs/kio/src/lock.rs:37-41): no
spin, no yield, no submit before blocking. The perf line already places the
fix in the shared model rather than the runtime; this quest only measures.

- Wrap the acquire in a `try_lock` fast path; on contention, time the
  blocking acquire and add it to a per-thread counter plus a contended-count.
  The fast path costs one atomic and is the common case, so the meter stays
  on in release builds.
- Expose `lock_wait` and `lock_contended` per io_uring worker on `/metrics`,
  beside `enters`, and per tokio worker where the same lock is used.
- Run the fanout and chat shapes with 1, 4, and 16 workers and report wait
  time as a share of worker CPU.

Below one percent, record it and close the quest. Above, open a quest with
the measured hot locks named, and only then decide between submitting staged
SQEs before a blocking acquire and shrinking the lock in the model.

## Related

- [Cache shard](/quest/m1/perf/cache-shard.md) - one of the shared cells the
  wait would point at
