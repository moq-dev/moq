# JSON codec properties

The Rust `moq-json` and JS `@moq/json` unit suites each run 256 Snapshot and 256 Window codec
sequences with the fixed seed `7171953`. They use maintained shrinking
libraries, proptest and fast-check. Both generate bounded JSON trees and
persistent-object edits, including nested nulls, empty objects, added and
removed keys, type changes, arrays, non-object roots, and prototype-named keys.
They route every emitted frame through the Decoder, commit it, and compare the
reconstructed JSON after every update, including updates that emit nothing.
Each sequence varies the delta ratio and explicit encoder resets.

Window traces contain pushes, clamped pops, explicit resets, op
ratios, and optional checkpoint bounds. A separate unbounded logical window
checks the exact push, pop, and skip events for readers following every group,
joining late, or dropping whole groups. The model never reads the encoder's
checkpoint suffix. It also checks that following readers retain the complete
window, pushes are unique, and skips never cover received records. Shared
`rs/moq-json/tests/window-vectors.json` fixtures pin both bytes and decoded events.

`just test json-properties` runs all four properties with 4096 cases and a fresh
seed, prints that seed, then sweeps JS codec cost over document size and changed
keys. Nightly runs this recipe. Replay a failure
with its printed settings:

```sh
MOQ_JSON_SEED=7171953 MOQ_JSON_CASES=4096 just test json-properties
```

Either setting can also be passed to the ordinary per-language unit suite.
Seed values are integers from 0 through 2147483647; case counts are positive
integers in the same range. The libraries print the shrunk input, including its
configuration, and fast-check also prints a replay path. Matching seeds do not
mean matching generated sequences across the languages.

A failure's fix keeps the minimized case: use the shared
`rs/moq-json/tests/vectors.json` when it reduces to a value pair; otherwise add a
codec sequence regression with its delta ratio and reset steps.
Do not replace the failing seed or weaken the property.

Window failures retain the minimized configuration, edits, reset flags, join
choice, and whole-group drop flags in a sequence regression. A shared frame
regression belongs in `rs/moq-json/tests/window-vectors.json`.
