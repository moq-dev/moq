# moq-net BTree build-cost experiment, 2026-10-26

Sharing the expiry wall index's numeric key removes one BTreeMap instantiation
and 6,485 LLVM lines. A quiet-host repeat on 2026-10-31 (below) confirms a real
but tiny instruction-count reduction (0.19% of the library build, 0.06% of the
unit-test build, about 30 ms of CPU) that CPU time cannot distinguish from
noise. The conversion is not retained; no production key representation changes.
At roughly 0.2% per instantiation removed, sharing BTree key and value types is
not worth its own work.

[Raw samples, type inventory, and exact candidate patch](btree-build-audit.json)
preserve the measured candidate and its context.

## Method

Base: [65ea670311](https://github.com/moq-dev/moq/commit/65ea670311d68058f5c3df74af669bc09c324c5c).
The repository's Nix shell supplies Rust 1.98.1, and its pinned nixpkgs supplies
cargo-llvm-lines 0.4.48 and GNU time. The host has 24 logical CPUs and is shared
with other builds. The host Cargo configuration disables dev-profile debug info.

Use a private `.tmp/target`, `MBX_DISABLE=1`, `CARGO_INCREMENTAL=0`, and
`CARGO_BUILD_JOBS=2`. Warm dependency builds first. Restore the selected expiry
source snapshot before each cohort so Cargo rebuilds the package. Measure in
baseline/candidate/candidate/baseline order, library then unit-test harness for
each cohort. Each command enters through `quest gate`; admission bounds host
load and memory, but does not reserve a quiet host.

The timed commands are:

```sh
cargo llvm-lines --locked -p moq-net --lib
cargo llvm-lines --locked -p moq-net --lib --profile test
```

The test profile selects the library's unit-test harness, including its test-only
features and dependencies. Do not append another `--test` rustc flag: Cargo
already supplies it. GNU time records process-tree user and system CPU for
cargo-llvm-lines, including compilation and IR analysis. These are neither
rustc-only CPU measurements nor estimates of merge-gate savings. Dependency
warm-up and admission waits are excluded from each timed command.

## Candidate and value domain

`model/expiry.rs` keeps five `BTreeMap<u64, kio::WaiterList>` indexes and a
separate `BTreeMap<Instant, kio::WaiterList>` for wall deadlines. The candidate
stores only that last map's keys as nanoseconds from `Instant::ZERO`, allowing
it to reuse the existing u64-keyed tree. Its public and call-site types remain
`Instant`; lookup complexity, map cardinality, and waiter ownership are unchanged.

The clock implementation stores `Instant` as exactly one private u64 nanosecond
offset. ZERO is its minimum. `duration_since(ZERO).as_nanos().try_into()` is
lossless for its entire domain, including zero and u64::MAX; there is no cast
that truncates a wider duration. Reconstructing with
`ZERO + Duration::from_nanos(key)` preserves every representable instant and its
ordering. Past instants are still nonnegative offsets from that origin. The
candidate adds no public clock accessor or range restriction.

This is a source-level range argument, not a completed runtime validation of a
retained change. The prototype was removed after measurement, so no runtime
behavior or new production test ships with this report.

## Results

| Cohort | Variant | Target | User CPU | System CPU | CPU total | Wall | LLVM lines |
| --- | --- | --- | ---: | ---: | ---: | ---: | ---: |
| A1 | baseline | library | 13.00 s | 0.79 s | 13.79 s | 13.82 s | 1,360,948 |
| A1 | baseline | unit tests | 48.91 s | 3.44 s | 52.35 s | 50.91 s | 3,292,164 |
| B1 | candidate | library | 15.63 s | 1.35 s | 16.98 s | 17.70 s | 1,354,463 |
| B1 | candidate | unit tests | 48.89 s | 4.03 s | 52.92 s | 50.91 s | 3,285,679 |
| B2 | candidate | library | 12.26 s | 0.95 s | 13.21 s | 13.31 s | 1,354,463 |
| B2 | candidate | unit tests | 46.90 s | 3.55 s | 50.45 s | 48.02 s | 3,285,679 |
| A2 | baseline | library | 11.85 s | 0.82 s | 12.67 s | 12.70 s | 1,360,948 |
| A2 | baseline | unit tests | 54.18 s | 3.74 s | 57.92 s | 59.90 s | 3,292,164 |

The IR reduction repeats exactly: 0.48% of the library and 0.20% of its unit-test
target. Rows naming BTree internals fall from 308,892 to 302,479 library lines
and from 364,034 to 357,621 unit-test lines. The baseline BTree share is 22.7%
for the library at this revision; the older compile-time audit used different
features and an earlier tree.

CPU ranges overlap in both targets. The library's candidate median is slower,
while the test target's is faster. The one-minute load sampled at command
boundaries ranges from 16.18 to 27.19 despite admission. Two observations per
variant cannot separate this small source change from the observed host
variation. No CPU speedup or regression is established, and no acceptance
threshold was tuned to these observations.

## Type inventory and remaining work

The raw inventory lists 38 concrete key/value spellings appearing in emitted
BTreeMap methods. It excludes a placeholder `BTreeMap<_, _, _>` row, includes
rangemap's internal trees, and represents BTreeSets by their `SetValZST` value.
It is a reproducible spelling inventory, not a count of every tree operation or
an attribution of all BTree lines to each pair. The candidate removes only
`Instant, kio::WaiterList`; the existing `u64, kio::WaiterList` remains.

Other candidates, such as sorted vectors for small counted priority/budget maps
or indexed values, need their own runtime fan-out and cardinality measurements;
this experiment does not justify them.

## Quiet-host repeat, 2026-10-31

Base: [51836997ce](https://github.com/moq-dev/moq/commit/51836997ce9352e3594f33a75d4ae69ec268bb95).
The preserved patch applies unchanged. Same toolchain (Rust 1.98.1, LLVM 22.1.8,
cargo-llvm-lines 0.4.48) and environment (`MBX_DISABLE=1`,
`CARGO_INCREMENTAL=0`, `CARGO_BUILD_JOBS=2`, dev debug info off, private
target), on a dedicated idle host: Intel Core i7-8700, 6 cores / 12 threads,
one-minute load 1.14 to 1.71 for the whole run.

Instead of `cargo llvm-lines`, each sample times a plain rebuild of moq-net
alone (touching its `lib.rs`) under `perf stat`, counting user-space
instructions, cycles, and task-clock over the cargo process tree:

```sh
cargo build --locked -p moq-net --lib
cargo test --locked -p moq-net --lib --no-run
```

Six rounds of baseline/candidate/candidate/baseline give 12 samples per variant
and target. The p-values are two-sided permutation tests on the difference of
means. Raw samples are in the JSON's `quiet_host_repeat`.

| Target | Metric | Baseline median | Candidate median | Mean change | p |
| --- | --- | ---: | ---: | ---: | ---: |
| Library | LLVM lines | 1,379,943 | 1,373,458 | -0.47% | exact |
| Library | Instructions | 78.609 G | 78.455 G | -0.19% | < 0.0001 |
| Library | Task clock | 16.35 s | 16.32 s | +0.61% | 0.32 |
| Unit tests | LLVM lines | 3,451,146 | 3,444,661 | -0.19% | exact |
| Unit tests | Instructions | 216.80 G | 216.65 G | -0.06% | < 0.0001 |
| Unit tests | Task clock | 47.36 s | 47.09 s | -0.32% | 0.40 |

The 6,485-line reduction repeats exactly. Instruction counts have a standard deviation under
0.03% between runs, so the drop of about 150 M instructions per target is real.
At this host's roughly 4.8 instructions per nanosecond of task clock, it is
worth about 30 ms per build. Task-clock spread (standard deviation 66 to 495 ms)
is larger than that, and the means move in opposite directions across targets,
so CPU time does not drop beyond noise.

Rows naming BTree internals are 332,566 of the library's 1,379,943 lines at this
base (24.1%). The measured candidate removed about 6.5 k of them, 0.19% of
library instructions and 0.06% of unit-test instructions, with no CPU-time
change distinguishable from noise. Other instantiations generate different
methods and amounts of code, so this one result does not bound them, but it gives
no reason to expect a large win from any single conversion, while each conversion
adds a key translation that callers and reviewers must keep lossless.

Decision: the conversion is not retained. The quest's own acceptance rule, a CPU
drop beyond noise, is not met by the cheapest candidate, and larger candidates
(sorted vectors, slab-indexed values) would trade runtime complexity for unmeasured
gains that this result gives no reason to expect beyond a few tenths of a percent
each. The maintainer
deleted the BTree keys quest on 2026-10-31 on this result.
