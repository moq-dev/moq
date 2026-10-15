# JSON snapshot properties

The Rust `moq-json` and JS `@moq/json` unit suites each run 256 Snapshot codec
sequences with the fixed seed `7171953`. They use maintained shrinking
libraries, proptest and fast-check. Both generate bounded JSON trees and
persistent-object edits, including nested nulls, empty objects, added and
removed keys, type changes, arrays, non-object roots, and prototype-named keys.
They route every emitted frame through the Decoder, commit it, and compare the
reconstructed JSON after every update, including updates that emit nothing.
Each sequence varies the delta ratio, compression, and explicit encoder resets.

`just test json-properties` runs the two properties with 4096 cases and a fresh
seed, prints that seed, then sweeps JS codec cost over document size and changed
keys, with and without compression. Nightly runs this recipe. Replay a failure
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
codec sequence regression with its compression, delta ratio, and reset steps.
Do not replace the failing seed or weaken the property.
