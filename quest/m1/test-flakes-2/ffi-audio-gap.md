# [S] FFI publisher audio gaps in interop

## Goal

The interop tone check (`test/interop/tone.py`, "timestamp gap") stops
failing for FFI publishers: `python -> c` and the `cpp ->` cells seen in
[moq-dev/moq#6464](https://github.com/moq-dev/moq/pull/6464) and
[moq-dev/moq#6648](https://github.com/moq-dev/moq/pull/6648).

## Plan

- [moq-dev/moq#6658](https://github.com/moq-dev/moq/pull/6658)'s max-age cause
  does not apply: FFI publishers keep hang's 30 s max-age.
- Reproduce under load and find which group or frame jumps, then fix the
  cause at its source.
