# Hub-crate test-layout inventory, 2026-10-26

Keep the current test layout until a coverage-preserving migration and CPU
comparison exist. A contained prototype moves four private-state cases into an
external harness and passes them, but also exposes an internal type through the
published `fuzz` feature. It leaves 2,715 other library unit tests unmigrated.
The original source, manifest, and test layout are restored.

No CI CPU saving is estimated. The structural inventory and mechanics probe
narrow the remaining work without treating a four-case prototype as a full
architecture or performance result.

[Inventory data, generator, compiler context, and exact prototype patch](test-layout-audit.json)
are pinned to
[main 5516a406f4](https://github.com/moq-dev/moq/commit/5516a406f4f680026e1765f89679a8cf7d362f8a).

## Structural inventory

The AST inventory covers 42 Rust packages with `rs/*/src/lib.rs`. It counts
source syntax across configuration branches, not one compiled feature set.
Test-owned lines are the union of spans requiring positive `cfg(test)`, test
functions, and modules reachable only through test-only declarations. Comments
and blank lines inside those spans count; macro-generated tests are not
expanded. Shared support guarded by `any(test, feature = ...)` is not itself
counted as test-only.

| Library | Source lines | Test-owned lines | Share | Syntactic test functions |
| --- | ---: | ---: | ---: | ---: |
| moq-net | 182,093 | 90,954 | 49.9% | 2,712 |
| moq-mux | 114,132 | 66,142 | 58.0% | 1,766 |
| moq-nvenc | 47,467 | 26,379 | 55.6% | 231 |
| moq-quic | 46,068 | 21,495 | 46.7% | 635 |
| moq-video | 46,633 | 17,280 | 37.1% | 453 |
| moq-tokio | 40,286 | 13,719 | 34.1% | 445 |
| moq-ffi | 21,486 | 10,961 | 51.0% | 270 |
| moq-relay | 22,182 | 9,574 | 43.2% | 285 |
| hang | 7,843 | 2,981 | 38.0% | 156 |
| kio | 5,845 | 2,516 | 43.0% | 100 |

moq-net is the largest structural candidate, and moq-mux is also substantial.
Neither source-line share establishes compile cost or makes either migration
worth adopting. Hardware/configuration-heavy crates need their active target
profile before ranking them by CPU opportunity.

### Private-item review inventory

In moq-net, 1,035 syntactic test functions have identifier matches to 342
restricted-visibility names in enclosing modules, across 75 files. This is a
review inventory, not compiler name resolution or a certified private-use
count. Shadowing and reexports can overcount; private fields, methods,
helpers, and macro expansion can undercount. The unmarked tests are not proven
to use only public API. The raw data preserves every candidate and its source
line so the remaining semantic review has a concrete starting point.

The largest test-owned source blocks are:

| File | Test-owned lines | Syntactic tests |
| --- | ---: | ---: |
| `ietf/publisher.rs` | 12,295 | 224 |
| `ietf/subscriber.rs` | 11,544 | 226 |
| `model/track.rs` | 11,306 | 407 |
| `model/origin.rs` | 6,287 | 206 |
| `lite/publisher.rs` | 4,680 | 106 |
| `model/resume.rs` | 3,997 | 112 |
| `lite/subscriber.rs` | 3,841 | 85 |

The compiled default-feature library harness registers 2,719 tests according
to `cargo nextest list --lib`. That count differs from the 2,712 syntax count
because configuration and test-generating macros belong to compilation. The
normal `just rs test` recipe passes `--tests`; adding `--lib` does not remove
its other selected test targets. Its baseline filter ran the four Delivered
cases among 3,259 tests across eight binaries. A separate library-only list
was used for the 2,719-unit-test count.

## Contained prototype

The prototype tests the mechanism on `model/delivered.rs`, whose four cases
check gap enumeration, merging neighbouring runs, floor advancement, and the
zero/u64::MAX sequence boundaries. Their assertions move to
`tests/layout_prototype.rs`; the helper that reads stored runs calls a temporary
getter instead of accessing the private field directly.

The exact preserved patch:

- sets `[lib] test = false`;
- exposes `Delivered` through `fuzz::test_layout`, making the otherwise private
  module reachable within the crate for that reexport;
- makes the existing test-only run-count method available under `fuzz` and adds
  one run-list getter for the existing assertions;
- adds the external four-case harness.

Both the original four cases and the external four cases pass through the
repository's nextest recipe. The external run selects one binary and skips
seven other integration binaries. Only four of the 2,719 unit tests are ported;
adopting this patch unchanged would omit 2,715 unit cases. It is not a complete
coverage-preserving migration. The prototype and all source/manifest changes
were then removed; only this report and its evidence remain.

### Visibility and feature cost

The prototype exports one internal type, its Default implementation, and ten
inherent methods through the existing public `fuzz` feature; two methods serve
test introspection. `doc(hidden)` hides documentation, not Rust visibility.
That is a public feature surface even though normal users are not expected to
call it. No such API change is retained here.

Four small cases already need type visibility, a field-inspection seam, and a
`cfg(test)` helper made available when the library is built as a dependency.
Larger model and wire suites also have test-only helpers and simulated-time
support. Moving their bodies outside the crate does not make those seams free.
A full prototype must distinguish public-API cases from cases needing an
intentional internal test surface, rather than reexport every implementation
type wholesale.

The existing `fuzz` feature implies `async`. It cannot be reused unchanged for
all model-only tests without weakening the no-default-feature boundary.
A dedicated test-support design, or keeping the affected cases inline, is a
maintainer choice. Its library helper dependencies must compile explicitly
under that design, rather than rely on `cfg(test)` or accidental workspace
feature unification.

## CI consumers and coverage

- `rs/justfile`'s normal test recipe compiles and runs `--tests`, enforcing clippy
  from the same build. Disabling the library test target removes it from that
  selection even while the remaining integration tests can pass.
- `sh/rs/net-features.sh` separately compiles and runs no-default-feature tests,
  checks runtime-free normal dependencies, and builds a browser leaf. Preserve
  that boundary; an async-only bridge is insufficient.
- `test/justfile`'s interop and bare-FIN recipes explicitly filter library test
  names. `rs/justfile`'s feed/fetch benchmark also targets an ignored library
  test. A migration must preserve or update these consumers in the same change.
- moq-net's Loom targets are already integration tests; kio's Loom recipe still
  names its library target. A repository-wide layout policy cannot assume the
  same shape for every crate.
- Existing wire/model integration targets, fuzz corpus replay, ignored cases,
  and platform-specific cases need an explicit before/after compiled test-name
  inventory. A passing selected subset is not evidence that all coverage moved.

## Measurement limit and recommendation

The mechanics runs used the Nix-pinned Rust 1.98.1 and its matching clippy driver
with the existing shared mbx cache. The baseline reported 165 hits/13 misses;
the prototype reported 0 hits/1 miss/10 not looked up/11 incremental. Host Cargo
configuration disables ordinary incremental builds, but mbx's existing state
was left enabled. Cache state was observed, not normalized for a CPU comparison.
The shared 24-logical-CPU host's load was observed at 22.61 during the session
and 11.57 after the prototype; these are not controlled per-run CPU samples.
Both runs waited for the Cargo package-cache lock; the baseline also waited at
quest admission and reported a missing predicted mbx blob. The preserved logs
replace only the enclosing absolute checkout path with `<checkout>`.
No edit-plus-tests CPU cohort or process CPU counters were collected.

Recommend preserving inline tests until a full migration can be measured on a
quiet host with mbx warm. Before that measurement, settle the test-support
surface and its feature boundary, migrate the complete selected set, and compare
compiled test identities across the same feature/platform configurations. Then
measure repeated edits with identical dependency/cache state and report CPU
variation alongside the effect. Do not extrapolate from source lines or the
four-case probe. moq-mux is the next structural candidate for profiling; this
report makes no CPU-based adoption recommendation for it.
