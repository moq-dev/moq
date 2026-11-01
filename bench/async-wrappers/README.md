# Async-wrapper generator spike

This checkpoint compares generation on `kio::Queue::pop` and all four wrappers in
`moq_net::broadcast`: `Producer::{requested_track, closed}`, `Consumer::closed`,
and `Demand::wanted_changed`. It does not pick a generator or change production
crates. The remaining work belongs to
[async-feature](/quest/m1/poll-first/async-feature.md).

Run in the repository's Nix shell:

```sh
quest gate python3 bench/async-wrappers/run.py --rounds 3
```

Use `--output .tmp/another-run` to keep earlier results. The runner refuses to
reuse variant directories. `--verify-only` skips timings on a fresh snapshot.
The source archive comes from `HEAD`, while the generator fixtures come from
beside the runner. Commit before measuring so the checkpoint is reproducible.
It requires the workspace lockfile's Cargo dependencies in the local cache;
`--offline` refuses a missing dependency instead of including downloads in timings.

The runner creates source snapshots under `.tmp/`, never edits the checkout,
and gives each variant its own target directory. It compares:

- **Handwritten:** the same five wrappers with poll docs copied to them, gated
  behind `async`. This controls for equivalent generated code and documentation.
- **Declarative:** `rules.rs` replaces each poll declaration with one macro
  invocation containing its docs, signature, and body. The explicit `poll => async`
  names avoid an identifier-concatenation dependency. The bracketed attributes
  apply only to the poll method, preserving `_sim`'s `track_caller`.
- **Attribute:** `attribute.rs` is appended to the existing kio-macros crate, and
  re-exported through kio only in the snapshots. A method attribute derives the
  wrapper name, return type, receiver, visibility, and docs from ordinary Rust.
  `cfg_attr(feature = "async", ...)` avoids building the generator in a standalone
  poll-only kio build. moq-net already enables kio's `macros` feature for machines.

Both prototypes intentionally support only safe, non-generic methods taking a
borrowed receiver and waiter. The enclosing impl may be generic (`Queue<T>`).
They are comparison fixtures, not ready-to-publish macro APIs. Generic methods,
extra arguments, explicit lifetimes, conditional method attributes, renamed
crate dependencies, and composition with `#[kio::machine]` still need design
and coverage after the maintainer chooses. An impl-level attribute would avoid
repeating `cfg_attr`, but would parse entire impls and need explicit opt-in for
which methods are wrappers. It is not measured here.

## Method

Three rounds rotate the variant order. Each round measures wall time for:

1. A clean standalone `cargo check --lib -p kio --no-default-features --features
   kio/async`, exposing the proc-macro dependency cost.
2. A fresh-target `cargo check --lib -p moq-net --no-default-features --features
   moq-net/async`, including its dependency graph.
3. The same moq-net command after an assertion-message edit in `model/broadcast.rs`, with
   incremental compilation enabled. This measures a recompilation of the changed
   method body, not an idle Cargo check, a comment-only edit, or an algorithm change.

The runner invokes Cargo/rustc/rustdoc from the pinned toolchain's sysroot,
disables compiler wrappers, shell startup injection, and custom Rust flags, uses two Cargo
jobs for every variant, and records all samples plus toolchain and source SHA in
`results.json`. OS file caches remain warm, and host contention can affect wall
time. This small spike measures generator overhead for five wrappers, not the
savings from eliminating every coroutine in the quest. Gating remains the
maintainer's settled direction regardless of speed.

Verification checks feature-on and feature-off library builds, Rustdoc with
warnings denied, and identical rendered poll/wrapper docs for `Queue::pop` and
`Producer::requested_track`. It also compiles an owned-receiver mistake and
requires failure; inspect `*-bad-receiver.log` for error spans. Other wrappers
and unrelated kio async bodies remain untouched, so a feature-off success here
does **not** establish the quest's eventual no-coroutines condition.

## rust2-driver

Upstream was inspected at
[06614b5a4577a9230206a5babf75c592b1e07efc](https://github.com/moq-dev/rust2/tree/06614b5a4577a9230206a5babf75c592b1e07efc).
Its [front end](https://github.com/moq-dev/rust2/blob/06614b5a4577a9230206a5babf75c592b1e07efc/driver/src/lib.rs)
and [compiler callbacks](https://github.com/moq-dev/rust2/blob/06614b5a4577a9230206a5babf75c592b1e07efc/driver/src/rustc.rs)
provide compiler integration, not async-wrapper generation. The driver uses
`rustc_private` and its own nightly toolchain; moq pins stable 1.98.1. A new
lowering would therefore be a compiler-toolchain project, including distributing
it to library consumers and supporting rustdoc and IDEs. No working lowering
was available to time or inspect for wrapper diagnostics/docs. This is an
unmeasured option requiring a larger spike, not a zero-cost measured alternative.

Rust's stable
[attribute macros](https://doc.rust-lang.org/reference/procedural-macros.html#attribute-macros)
can emit adjacent methods and preserve input spans. Stable
[declarative macros](https://doc.rust-lang.org/reference/macros-by-example.html)
can also emit both methods, but consume a dialect rather than annotate a normal
method. Both fixtures copy docs from the poll declaration instead of maintaining
a second signature/doc list.
