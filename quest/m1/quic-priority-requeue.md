# [S] Changing a queued stream's priority moves it

## Goal

`SendStream::set_priority` in moq-quic takes effect at once for a stream
waiting in its group's queue (`Queued::At`), as `SendGroups::set_priority`
does for a group, so raising a starved stream's priority lets it send. A
stream finishing a frame (`Queued::Frame`) keeps its place and takes the new
priority when it next requeues.

## Plan

Found in the 2026-10-23 moq-quic audit. `set_priority`
(`moq-quic/src/connection/streams/mod.rs`) only stores the new value; the
schedule key is fixed by `Schedule::push` (`streams/schedule.rs`), which
returns early for a queued stream. A stream waiting at priority 0 behind
priority-5 streams that keep requeueing is never popped, so raising it to 10
never applies. moq-net changes stream priority at runtime in both the lite
and IETF publishers, so this is live.

When the slot is `Queued::At`, remove it and push it again at the new
priority (O(log n)); the "takes its new priority the next time it is
requeued" doc goes. Keep a stream that is finishing a frame (queued ahead of
its group) where it is.

Test: a starved queued stream whose priority is raised sends next; lowering
one moves it behind its peers at once.

## Related

- [Streams API](/quest/m1/quic-api/streams.md) - renames the method to `send::Stream::set_priority`
