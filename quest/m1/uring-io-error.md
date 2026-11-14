# [XS] An io_uring I/O error names its cause once

## Goal

`moq_uring::Error::Io` shows its `std::io::Error` once when printed with
anyhow's `{:#}` or walked as a source chain. Today it displays
`"io error: {0}"` and also makes that error its `#[from]` source, so the
relay's startup failure prints the cause twice.

## Plan

Decided 2026-11-13 (maintainer, /quest-run follow-up from
[moq-dev/moq#8642](https://github.com/moq-dev/moq/pull/8642)): ✅ m1 (rejected: m0).

Recommended: keep the variant and `#[from]`, and drop `{0}` from its message.
Check the callers that print a `moq_uring::Error` with plain `{}`, since they
would then lose the cause; switch them to `{:#}` or a chain where they do.
Only the Display text changes, so no API breaks.
