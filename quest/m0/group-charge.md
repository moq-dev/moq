# [M] Group charge

## Goal

The group cache charges what a cached group actually costs, so
`MOQ_CACHE_CAPACITY` bounds real memory. It undercounts chat-shaped traffic
badly enough today that a relay is OOM-killed while its pool still believes it
is inside budget.

## Plan

`cache::ENTRY_OVERHEAD` charges 256 B per cached group on top of frame payload,
so a group holding one 150 B frame (the chat shape) is billed 406 B against a
then-measured 1,637 B of real RSS: a 4x undercount.

The gap is `group::Producer`'s kio state cell. Video is unaffected: a group
carrying many frames is dominated by payload, and the 256 B is close enough
there. This is specifically one-frame-per-group traffic.

Concretely, on a 1 GB nanode with `MOQ_CACHE_CAPACITY=50%`: 512 MB of budget at
406 B per group is ~1.26M groups. The pool evicts nothing until it thinks it is
full, so the process dies first.

Remeasure the real cost before touching the constant. The 1,637 B was taken when
a `kio::State<()>` was 896 B and it is 200 to 224 B today, so a group should be
roughly 672 B cheaper now. That still leaves it well over the 406 B charged, and still
over a 1 GB box at 1.26M groups, so the constant still has to move; only the
multiple is unknown. Derive it from `size_of` rather than pasting a measured
number, so it tracks the structs instead of rotting: a wrong `ENTRY_OVERHEAD` is
silent in both directions.

`ENTRY_OVERHEAD` also bounds the live group count (`used / 256`) for the
access-time sum in `TICK_MS`; raising it only loosens that bound, but re-check
the overflow arithmetic in the comment and update it.

`kio`'s `tests/waiter_allocs.rs` and `the_list_stays_small` are the pattern to
copy for keeping the derivation honest: assert the footprint rather than
describing it.
