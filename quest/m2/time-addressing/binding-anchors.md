# [M] Use Time for group positions throughout the native bindings

## Goal

The native SDKs and C expose one timestamp anchor, Time. Its start selects the group containing that time and its end includes groups starting at or before that time. Exact group fetch stays available by start key.

## Plan

Decided in the release audit interview: ✅ combine Absolute and Time and use the Rust and JS inclusive time end. The FFI and C already lower both starts to Time after [moq-dev/moq#8638](https://github.com/moq-dev/moq/pull/8638). Their exclusive Absolute end only subtracts one tick. Repository callers found using it were tests. Rejected: retaining both spellings, adding a lower-bound start, or introducing a public exclusive-end option.

With group starts 1000 and 2000, Time(1500) starts in group 1000 and an end of Time(2000) includes group 2000. Migrate former exclusive ends at positive t to Time(t - 1) in track ticks. An exclusive end at zero requests no groups: preserve an empty selection in the migration example and test it separately from omitted ranges (the default subscription). Relative counts and the oldest start keep their existing roles.

Change the UniFFI and hand-written C surfaces, wrappers, tests and docs together. Identify old start/end forms in the PR Impact section. Preserve stable-wire behavior and document any negotiated-version limitations.
