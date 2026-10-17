# Migrating moq-mux

How to update your code for each breaking change, newest first.

## Unreleased

### `ts::Programs::finish` returns each program's `Finish` (#5795)

`Programs::finish` returns `anyhow::Result<Vec<moq_net::broadcast::Finish>>` instead of `anyhow::Result<()>`.
Each handle keeps its program's broadcast servable until its media is stale, and dropping it closes that program.
Await `Finish::wait` to linger, or drop the handles to close every program now.

```rust
// Before
programs.finish()?;

// After: serve the finished programs until their media is stale
let finished = programs.finish()?;
futures::future::join_all(finished.into_iter().map(moq_net::broadcast::Finish::wait)).await;

// After: close every program now
drop(programs.finish()?);
```
