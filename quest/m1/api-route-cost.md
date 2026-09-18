# [S] Route construction names cost parts and hop chains

## Goal

`Route::with_hop` and `Cost: From<(u64, u64)>` are gone. The only in-tree
callers that still need both magnitudes and a hop chain (moq-ffi's
`TryFrom<MoqRoute>`, libmoq's `parse_route`) build a `Hops` then
`Route::with_hops`, and name the two cost halves with a constructor.
Everyone else already uses `with_hops`.

## Plan

`Cost` is `#[non_exhaustive]`, so ffi and libmoq cannot write a struct
literal. Add `Cost::from_warm_cold(warm, cold)` for a discounted warm
alongside its undiscounted cold, the meaning the tuple `From` has today.
Keep `Cost::new` / `From<u64>` for an undiscounted route.

In `rs/moq-ffi/src/origin.rs` and `rs/libmoq/src/api.rs`, collect hops with
`Hops::push` (same `InvalidHop` as `with_hop`) and
`Route::default().with_cost(Cost::from_warm_cold(warm, cold)).with_hops(hops)`.
Then delete `Route::with_hop` and `impl From<(u64, u64)> for Cost`. Wrappers
do not construct `moq_net::Route` themselves; only those two parsers do.

Public API: breaking on moq-net, so on dev. Wire: none. The C/UniFFI route
structs are unchanged.

## Related

- [libmoq units](/quest/m1/api-libmoq-units.md) - other C ABI breaks on the same crates
