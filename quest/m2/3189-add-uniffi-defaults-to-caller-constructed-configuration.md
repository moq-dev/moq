# [S] Add UniFFI defaults to caller-constructed configuration records

## Goal

Implement and verify the behavior tracked in [#3189](https://github.com/moq-dev/moq/issues/3189)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

#### Problem

Several caller-constructed UniFFI records describe fields as optional or defaultable but do not declare UniFFI defaults:

- [`MoqAudioEncoderOutput` and `MoqAudioDecoderOutput`](https://github.com/moq-dev/moq/blob/7494084aaf7e2fa6abe553ac83101ac4ef19f33a/rs/moq-ffi/src/audio.rs#L68-L100)
- [`MoqVideoHint`, whose documentation says every field is optional](https://github.com/moq-dev/moq/blob/7494084aaf7e2fa6abe553ac83101ac4ef19f33a/rs/moq-ffi/src/media.rs#L159-L176)

The video encoder and decoder output records already use `#[uniffi(default = None)]` consistently:

- [Video output record defaults](https://github.com/moq-dev/moq/blob/7494084aaf7e2fa6abe553ac83101ac4ef19f33a/rs/moq-ffi/src/video.rs#L97-L114)
- [Video decoder defaults](https://github.com/moq-dev/moq/blob/7494084aaf7e2fa6abe553ac83101ac4ef19f33a/rs/moq-ffi/src/video.rs#L375-L388)

Python, Swift, Kotlin, and Go publicly expose or alias the generated records. Without generated-language defaults, callers must spell every optional field, and adding a new field can become a source compatibility break.

#### Proposed direction

Audit every caller-constructed UniFFI record and declare defaults for all fields whose omission has defined behavior. At minimum:

- optional values should default to `None`
- audio encoder frame duration should use its documented 20 ms default
- enum defaults should be declared where the API already defines a canonical automatic choice

Keep returned data records distinct from caller-constructed options. Returned records do not need construction defaults.

#### Acceptance criteria

- Optional configuration fields can be omitted in Python, Swift, and Kotlin.
- Generated signatures expose the intended defaults rather than only documenting them.
- Adding a new optional field to these records remains source-compatible where UniFFI supports it.
- Binding tests construct each options record with only its required fields.
- The audit covers all public records in `moq-ffi`, not only the three known examples.

## Closes

- [#3189](https://github.com/moq-dev/moq/issues/3189) - close this issue when the quest finishes

## Related

- [#3208: Make 2.5 ms Opus frame durations work across bindings](/quest/m1/3208-make-2-5-ms-opus-frame-durations-work-across-bindings.md) - related open work
