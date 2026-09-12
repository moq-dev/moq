# [S] Align readable input types with accepted Getter implementations

## Goal

A value accepted as a component's readable input by TypeScript either works
at runtime or is rejected by the public type before the component is built.

## Plan

At dev `e2350b39a`, `GetterInit<T> = T | Getter<T>` advertises structural
readables (`js/signals/src/index.ts:318`), but `getter` rejects unbranded
implementations (`:329-338`). `io.test.ts:98` deliberately constructs a typed
foreign Getter and expects a throw. The audit ran this suite: 17 tests passed,
including that mismatch. `Derived` helps map values but does not make the
advertised input protocol accurate for external reactive adapters.

Decided: accept any conforming Getter without wrapping it or installing an
unowned subscription.
Keep cross-package-version Signal compatibility and distinguish readable
objects from ordinary data deliberately; do not silently freeze a readable.

Add compile-time and runtime examples for foreign adapters, built-in readers,
read-only component outputs, and plain values. Verify notifications and
unsubscribe behavior, including a queued change at subscription time. Update
the existing rejection test to encode the chosen contract, and document it
on GetterInit, Inputs, and getter together.

Public API: accepted readable types/behavior change. Wire: none. Run the
existing signals tests and JS declaration builds.
