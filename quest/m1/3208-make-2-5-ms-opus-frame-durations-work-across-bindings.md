# [L] Make 2.5 ms Opus frame durations work across bindings

## Goal

Implement and verify the behavior tracked in [#3208](https://github.com/moq-dev/moq/issues/3208)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

#### Problem

Native Rust supports every Opus frame duration, including 2.5 ms, and tests it. Public binding surfaces advertise the same support but cannot represent or carry it correctly:

- UniFFI exposes `frame_duration_ms: u32` and converts with integer milliseconds, so 2.5 is unrepresentable: [audio.rs](https://github.com/moq-dev/moq/blob/7494084aaf7e2fa6abe553ac83101ac4ef19f33a/rs/moq-ffi/src/audio.rs#L68-L79)
- C has the same integer representation and conversion.
- JavaScript accepts a floating-point millisecond value, then routes it through an integer catalog field and rejects 2.5.
- The native encoder validates exact durations of 2.5, 5, 10, 20, 40, and 60 ms.

Rounding to 2 or 3 ms is not valid because the Opus backend rejects both.

This overlaps #3189 only at the 20 ms default. The representational bug is cross-surface and should be fixed independently.

#### Settled API

Keep the concise `frame_duration_ms` spelling:

- UniFFI: `f64`, default `20.0`
- C: `double`, with zero selecting the 20 ms default
- JavaScript: retain floating-point `Time.Milli`

Validate that the value is exactly one of `2.5, 5, 10, 20, 40, 60`.

#### Implementation plan

1. Change the FFI and C scalar types to floating-point milliseconds and perform exact supported-value validation before converting to `Duration`.
2. Add the UniFFI 20 ms default in coordination with #3189 so the field is changed once.
3. Preserve the exact JavaScript frame duration separately from catalog metadata.
4. Encode catalog jitter as the ceiling of the duration because jitter is an integer upper-bound hint, not the exact encoder cadence.
5. Update Python, Swift, Kotlin, Go, and C documentation and examples generated or wrapped from these types.
6. Add 2.5 ms regression tests for FFI, C, and JavaScript, plus default-construction coverage.

#### Branch targeting

Changing the published FFI and C field types is source-breaking, so this targets `dev`.

## Closes

- [#3208](https://github.com/moq-dev/moq/issues/3208) - close this issue when the quest finishes

## Related

- [#3189: Add UniFFI defaults to caller-constructed configuration records](/quest/m2/3189-add-uniffi-defaults-to-caller-constructed-configuration.md) - related open work
