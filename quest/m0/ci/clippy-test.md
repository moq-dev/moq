# [S] Test enforces Clippy so Check stops repeating it

## Goal

The Clippy lints that gate a merge are enforced once, during the compile that
Test already does, and Check no longer runs a second default-feature Clippy
pass. Docs and the distinct feature and target checks still run in Check, in
parallel.

## Plan

Test already compiles through clippy-driver, but warnings do not fail it, and
Check's separate pass cost nearly five minutes on the default suite in one
sampled run. Deny warnings in Test's compile. Verify the two passes cover the
same targets and lints before removing Check's; any target only Check lints
(benches, examples) stays.
