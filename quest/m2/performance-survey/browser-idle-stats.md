# [M] Local bandwidth sampling follows demand

## Goal

Browser connections avoid unnecessary local transport-stat work while
preserving useful send-rate estimates when publishing needs them.

## Plan

[Reload](/js/net/src/connection/reload.ts) samples `connection.stats()` every
100 ms whenever established, independently of active bandwidth reservations.
This includes receive-only connections and pooled sessions during linger.
[QMux stats](/js/net/src/connection/stats.ts) still allocates an empty result
when the transport has no `getStats`. Remote PROBE observation is separate.

In the real browser harness, sweep distinct connections × active publishing
reservations. Include zero reservations, receive-only playback, active/idle
transitions, pool linger and QMux. Identical pooled handles are a control,
not distinct network connections. Count timers, real `getStats` calls,
allocations/GC and main-thread CPU; measure first useful estimate latency
after publishing resumes. Keep current cadence as the baseline before
choosing demand gating or any changed sampling policy.

Follow the [survey benchmark contract](/quest/m2/performance-survey/README.md):
baseline first, paired evidence, equivalent work and nightly coverage. A
measured no-win is valid. No API or wire change is expected; report actual impact.
