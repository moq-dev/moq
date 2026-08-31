# [M] Group charge

## Goal

The group cache charges what a cached group actually costs, so
`MOQ_CACHE_CAPACITY` bounds real memory. It undercounts chat-shaped traffic by
4x today, which means a relay is OOM-killed while its pool still believes it is
inside budget.

## Plan

`cache::ENTRY_OVERHEAD` charges 256 B per cached group on top of frame payload.
Measured with one 150 B frame per group, which is the chat shape:

| | Real RSS per group | Pool charges |
|---|---|---|
| today | 1,637 B | 406 B |
| after [waiter slots](/quest/m2/relay-memory/waiters.md) | 917 B | 406 B |

The gap is `group::Producer`'s kio state cell, which [waiter
slots](/quest/m2/relay-memory/waiters.md) shrinks but does not remove. Video is
unaffected: a group carrying many frames is dominated by payload, and the 256 B
is close enough there. This is specifically one-frame-per-group traffic, the
chat shape measured above.

Concretely, on a 1 GB nanode with `MOQ_CACHE_CAPACITY=50%`: 512 MB of budget at
406 B per group is ~1.26M groups, whose real cost is ~2.1 GB. The pool evicts
nothing until it thinks it is full, so the process dies first. After waiter
slots it is ~1.15 GB, still over, which is why the constant has to move too.

Land after [waiter slots](/quest/m2/relay-memory/waiters.md) so the constant is
set against the post-fix per-group cost rather than being written twice. Derive
it from `size_of` rather than pasting a measured number, so it tracks the
structs instead of rotting: a wrong `ENTRY_OVERHEAD` is silent in both
directions.

`ENTRY_OVERHEAD` also bounds the live group count (`used / 256`) for the
access-time sum in `TICK_MS`; raising it only loosens that bound, but re-check
the overflow arithmetic in the comment and update it.

Can share the [waiter slots](/quest/m2/relay-memory/waiters.md) PR if that one
is still open, since both are `moq-net`/`kio` changes.

## Required

- [Waiter slots](/quest/m2/relay-memory/waiters.md) - sets the per-group cost the constant has to match
