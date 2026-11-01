# Generator choice checkpoint

Recommendation for the maintainer: use a method attribute in the existing
kio-macros crate. This is a recommendation, **not a settled choice**. Keep the
[async-feature quest](/quest/m1/poll-first/async-feature.md) and its PR open until
the maintainer picks a generator and the rollout finishes.

## Measurements

Measured on 2026-11-01 from
[263e56b0fcabce1402cc4a405bad06bfa22c8fa3](https://github.com/moq-dev/moq/commit/263e56b0fcabce1402cc4a405bad06bfa22c8fa3),
with pinned Rust 1.98.1 on Linux x86\_64, two Cargo jobs, separate fresh target
directories, compiler caches bypassed, and three rotated rounds.
[Raw samples](results.json) retain the exact toolchain and source identity.
These are `cargo check --lib` wall times, not release builds or runtime benchmarks.

| Variant | kio clean, seconds | moq-net clean, seconds | moq-net body edit, seconds |
| --- | ---: | ---: | ---: |
| Handwritten control | 1.749 (1.265–2.010) | 28.966 (24.279–36.841) | 1.993 (1.754–2.001) |
| Declarative | 1.609 (1.402–2.114) | 24.447 (23.468–37.555) | 1.751 (1.610–2.241) |
| Attribute | 5.302 (4.591–6.470) | 29.567 (27.239–35.154) | 1.721 (1.461–2.089) |

Cells show median (minimum–maximum). Host contention and warm OS caches remain:
moq-net's ranges overlap, and even the handwritten control's clean checks span
12.562 seconds. Do not rank moq-net build speed from this sample. The attribute's
standalone kio cost is clear: its clean median adds 3.553 seconds by enabling the
existing syn/quote/machine macro graph. moq-net already builds that graph. The
five-wrapper spike does not measure whole-library coroutine removal savings;
the maintainer already decided that gating proceeds regardless of those savings.

Reproduce with the [runner and method](README.md). The successful command was:

```sh
quest gate python3 bench/async-wrappers/run.py --rounds 3 --output .tmp/async-wrappers-run3
```

## Diagnostics and docs

All three variants passed kio and moq-net library checks with the selected
wrappers' feature enabled and disabled. Rustdoc passed with warnings denied.
The actual rendered docs for `Queue::pop` and `Requests::requested_track` match
their poll methods exactly in every variant, including resolved links.

An owned receiver deliberately substituted for `&self` produces these diagnostics
in the generated snapshots:

| Variant | Primary diagnostic | Span |
| --- | --- | --- |
| Handwritten | `cannot move out of *self, as self is a captured variable in an FnMut closure` | The wrapper's `self.poll_pop(waiter)` call |
| Declarative | `no rules expected keyword self` | `self` in the macro invocation; a note points to the matcher's `&` |
| Attribute | `wrapper requires a borrowed receiver` | `self` in the original Rust method |

The runner requires both failure and the expected diagnostic, then restores the
input. The attribute can report its contract directly; the declarative version
can improve its messages with catch-all rules, at the cost of more dialect code.

## Options for the maintainer

1. **Method attribute, recommended.** Keep canonical poll methods as ordinary
   Rust, derive the async name/signature/docs, and reuse kio-macros. This avoids a
   second signature list and keeps method bodies readable to Rust tooling before
   expansion. Accept the measured standalone kio async-build cost. Generalize the
   prototype for the real signature inventory and test macro composition before
   publishing it; this checkpoint does not claim IDE completion was tested.
2. **Declarative macro.** Avoid the standalone proc-macro cost. The prototype is
   smaller (26 lines versus 118 for the attribute), but moves the poll declaration
   and body into a dialect and spells both method names. A separate wrapper
   signature list cannot read poll docs; that duplicate source was deliberately
   dropped. Stable 1.98.1 rejects `${concat(...)}` with E0658. This remains viable
   if minimum kio build cost outweighs ordinary Rust source and targeted errors.
3. **rust2-driver lowering.** Fund a larger compiler spike first. The pinned
   [upstream inspection](README.md#rust2-driver) found no wrapper lowering to
   measure. Its nightly `rustc_private` integration would need consumer,
   rustdoc, and IDE delivery work. No timing, diagnostic, or docs result is
   claimed for a nonexistent implementation.

No newer settled generator choice was found in the current quest or the merged
[machine-bodies PR](https://github.com/moq-dev/moq/pull/7284). Its straight-line
machine conversion is present in the spike's base. No public library API, default
features, wire format, bindings, or JS API changes in this checkpoint.

## Checks and remaining work

- The full spike passed: all feature builds, Rustdoc/doc comparisons, expected
  diagnostics, and nine timed variant rounds.
- `quest check --base origin/main` passed (651 documents).
- `just ci origin/main` passed with pinned Nix binaries, `BASH_ENV=/dev/null`,
  and a scratch `CARGO_TARGET_DIR` independent of the host mbx target symlink.
  The ordinary host invocation first failed in the existing test-lint fixture
  because mbx returned `Argument list too long`; the same host failure is already
  recorded in [moq-dev/moq#7464](https://github.com/moq-dev/moq/pull/7464). The
  fixture also passed independently with the pinned toolchain. No repository
  workaround was added for the host launcher.
- Local author review checks the snapshots, feature wiring, diagnostic spans,
  rendered docs, raw samples, and the limits of the performance claim. Independent
  cross-vendor review remains for the implementation after the maintainer's pick.

After the pick: design the final macro contract against the full signature
inventory, put its verification in CI, convert remaining async logic to polls,
roll out generated wrappers and default-disabled `async` features, enable them at
application/test/binding edges, and verify library-only builds contain no
coroutines. Keep the migration notes and instruction/doc updates with that
rollout. These remain work in this quest, not completed work in this checkpoint.
