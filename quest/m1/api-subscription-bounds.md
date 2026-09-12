# [M] Give local and requested subscription ranges consistent ends

## Goal

A caller can express an empty range and translate requested delivery bounds
to local filtering without off-by-one conversions or extra delivered groups.
Keep local filtering distinct from aggregated upstream demand.

## Plan

At dev `e2350b39a`, `track::Subscription::end` is an exclusive `Position`
(`rs/moq-net/src/model/subscription.rs:66`), but `Subscriber::end_at` and
`Ordered::end_at` take an inclusive group number (`model/track.rs:3569,3717`).
`Position::before` cannot represent anything below group 0. The C adapter
explicitly acknowledges this and maps an empty end to inclusive group 0
(`rs/libmoq/src/consume.rs:633`), relying on upstream demand to suppress it.
That is inadequate as a local range contract, especially with other readers
keeping wider aggregate demand alive.

Decided: use exclusive ends consistently, including local group/frame caps.
Preserve the
ability to raise/remove a cap without skipping held groups. Do not conflate
moving a local cursor with requesting historical data.

Audit group caps, Ordered forwarding, Lite/IETF adaptations, C raw reads,
JS equivalents, and all language range wrappers. Test empty ranges, group 0,
frame-limited ends, unbounded ends, maximum positions, and narrowing/widening
while another subscriber requests everything. Put regressions in existing
CI suites and run cross-language smoke.

Public API: likely breaking cap signatures or semantics. Wire: preserve the
existing exclusive wire encoding; update the matching draft only if the
chosen behavior changes what peers observe.
