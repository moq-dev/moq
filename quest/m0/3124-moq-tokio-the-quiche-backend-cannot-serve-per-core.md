# [M] moq-tokio: the quiche backend cannot serve per-core workers, so io_uring has no matched control…

## Goal

Implement and verify the behavior tracked in [#3124](https://github.com/moq-dev/moq/issues/3124)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

Trying to measure what `--runtime-io-uring` actually buys, there is no way to hold the QUIC stack constant across the comparison, because the two axes are welded together:

- `--runtime-workers N` on tokio requires the **quinn** backend.
- `--runtime-io-uring` is **quiche** (sans-IO quiche on the ring).

`--runtime-workers 4 --listen-backend quiche` is refused outright:

```
Error: failed to start the QUIC workers

Caused by:
    the quiche backend cannot serve per-core workers; use the quinn backend
```

from `rs/moq-tokio/src/quiche.rs`:

```rust
/// This backend cannot encode the owning worker in its connection IDs, so a
/// reuseport group built on it would have no way to steer packets back.
#[error("the quiche backend cannot serve per-core workers; use the quinn backend")]
ShardUnsupported,
```

So every io\_uring-vs-tokio number confounds the runtime with the QUIC implementation, and there is no fourth cell to separate them.

#### Why it matters

The confound is not small. Measuring the one control that *is* available (both backends on the shared runtime, 1:200 video fan-out, 60fps, 4 KB frames, `dev` @ `fc57e0175`, n=2):

| shared runtime | relay cores | Mbps | us/frame | RSS MB |
|---|---|---|---|---|
| quinn | 0.622 | 298.8 | **76.69** | **259.2** |
| quiche | 1.182 | 339.1 | **128.36** | **65.6** |

The two backends differ by 67% on CPU per frame and 4x on memory, in opposite directions. Which means the headline result people will quote from the io\_uring work - that it uses a fraction of the memory - is mostly quiche's doing, not io\_uring's: quiche on the plain shared tokio runtime already gets RSS to 65.6 MB, against 55.0 MB for io\_uring. Meanwhile the *runtime* is what rescues quiche's CPU cost: 128.36 us/frame on shared tokio versus 58.37 on the io\_uring workers.

Both of those are worth knowing separately, and right now the tree cannot express either.

#### Suggestion

`moq-uring`'s `quic::Endpoint` already solves exactly this problem - `endpoint::Config::shard` makes every issued connection ID lead with the steering byte so a `SO_REUSEPORT` group routes back to the owning worker (#3078). Teaching the tokio quiche backend the same trick would both remove an arbitrary restriction and give the io\_uring work a matched control to be evaluated against.

If that is more than it is worth, it would at least help to say plainly in `doc/bin/relay/config.md` that the io\_uring and tokio worker modes are not the same QUIC stack, so the comparison is read correctly.

## Closes

- [#3124](https://github.com/moq-dev/moq/issues/3124) - close this issue when the quest finishes
