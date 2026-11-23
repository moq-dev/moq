# [S] A session's taken-frame shed also drops inflated copies readers took

## Goal

When a session is past its `live_edges` budget, the first reclaim pass (`cache::Edges::reclaim`, `GroupState::shed_taken`) sheds the inflated frames a plain reader of a received compressed edge already took, along with their stored slices. That keeps a higher-priority compressed edge's read content from making the whole-edge pass evict a lower-priority reader's owed frames. It must never drop an inflated frame a reader is still owed, and never one an in-flight read has yet to take.

## Plan

Today `shed_taken` sheds only stored slices. The plain copy's inflated frames keep billing the session until the whole-edge pass or `MAX_CACHE_BYTES` trims them.

A first attempt, in [moq-dev/moq#9439](https://github.com/moq-dev/moq/pull/9439)'s review, popped plain frames below the shed slice index. Codex found two flaws in it:

- `Prefetch::fill` advances pace before `Prefetch::inflate` takes the job's result, and `Inflate::run` settles the bill in between. So a reclaim can drop the active read's inflated frames as taken, and that reader gets `Evicted`.
- A long compressed group trims its slices while keeping their inflated frames for a plain reader (`a_long_compressed_group_sheds_its_front`). Bounding by the slice front sheds frames past `read`.

Recommended: bound by each inflated entry's own `end`, which `Inflate` copies from its slice (`entry.end <= read`). Also pin the frames of inflate jobs in flight in `PlainState`, for example the lowest index a running job keeps, as `Inflate::keep` does for the `MAX_CACHE_BYTES` trim. Test it with mock time: a plain reader mid-prefetch, a session past its budget, and a second compressed edge.

## Related

- [Pool pinned debt](/quest/m1/pool-pinned-debt.md) - the pool-side bound on live edges
