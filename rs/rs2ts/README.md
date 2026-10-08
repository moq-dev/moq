# rs2ts prototypes

Executable experiments for the [translator quest](/quest/m1/rs2ts/translator.md).
This is not the production translator or a published package. The production
tool is still planned as Rust on `charon_lib`; the small Python emitter here
lets us test LLBC mappings before committing to that implementation.

## Run

From the repository root, inside the Nix shell:

```sh
just test rs2ts
just --justfile rs/rs2ts/prototype/justfile bench
```

The check compiles a native Rust oracle, runs TypeScript against it, checks
types and formatting, and verifies the generated snapshot. Nightly runs the
same recipe. Charon is not required to check the committed output.

## What actually translates

`prototype/generated.ts` comes from moq-net's real `varint::zigzag` and
`varint::unzigzag`, extracted by Charon 0.1.284 at commit
[`d3c5b7da240f728ebd317891f79f04770ef7042e`](https://github.com/AeneasVerif/charon/tree/d3c5b7da240f728ebd317891f79f04770ef7042e),
using nightly-2026-09-17, `--precise-drops`, and `--sysroot default`.
The snapshot retains the extraction roots without their unrelated declarations.
Their original source text is checked against moq-net so source edits require
regeneration.

The emitter recognizes scalar types and MIR operations, with no moq-net names
or function-body templates. `u64` and `i64` use immutable `U64` halves; signed
values use their two's complement bit pattern. Signed shifts extend the sign,
casts between the two 64-bit types preserve bits, and narrow constants are
range checked before emission. The generated arithmetic contains no BigInt.

The accepted subset is deliberately small: straight-line, single-assignment
scalar functions, copies/moves of immutable scalars, integer literals, integer
comparisons, shifts, XOR/AND, wrapping signed negation, the demonstrated casts,
and assertions with no cleanup work. `StorageLive`/`StorageDead` are irrelevant
only because every accepted local is a scalar without drop glue. Scalar panic
edges throw a JavaScript error; this does not emulate process abort or Rust UB.

Unsupported types, calls, mutation, branches, generic functions, unsafe
functions, and nontrivial unwind cleanup are refused during generation.
Partial extractions and other Charon versions are also refused. Diagnostics
name the Rust function and source position. No runtime `todo()` is emitted.

With Charon's pinned toolchain available, regenerate with:

```sh
just --justfile rs/rs2ts/prototype/justfile extract
just test rs2ts
```

`RS2TS_CHARON` can name an installed Charon binary; otherwise `charon` is used.
Charon's [installation instructions](https://github.com/AeneasVerif/charon#installation--build)
describe its Nix package. Extraction uses `--start-from` on the two codec
functions, and writes the raw extraction and compiler artifacts to `.scratch/`.

## Semantic probes

The native oracle checks 15,900 integer cases: boundary values through
`u64::MAX`, 256 deterministic pseudorandom values, addition/subtraction with
overflow, bit operations, shifts across 31/32/33 bits, signed negation, and
zigzag. Both generated functions are compared with native Rust independently
of the handwritten runtime experiments.

The lifetime and state probes are **handwritten TypeScript**, not generated:

- A tagged nested option preserves `None`, `Some(None)`, and `Some(Some(v))`.
  Erasing both layers to `undefined` loses a state.
- Explicit owners preserve clone/move/last-drop behavior, early return, reverse
  lexical destruction, and guards against double drop and use after move.
- Nested `finally` blocks reproduce cleanup order during a panic. That alone
  does not prove the emitter can lower arbitrary MIR unwind graphs.
- Copying a struct's fields preserves Rust's independent mutation. Ordinary
  object assignment would alias the original. A generic copy operation must
  follow field types; a shallow spread is only enough for this scalar struct.

These probes support keeping integers exact and deriving ownership/copy
operations from MIR, rather than assuming JavaScript object identity matches
Rust values. They do not settle the public handle API or change moq-net.

## Measurements

An initial Bun 1.4.2 run on an Intel Core Ultra 7 270K Plus, with other work
running on the machine. Times are nanoseconds per checked add plus zigzag,
over 256 varied inputs. Each representation runs 500,000 iterations per
sample, with two warmup rounds and seven measured rounds, rotating run order.

| Range | Handwritten halves | Generated halves | BigInt |
| --- | ---: | ---: | ---: |
| Small | 17.8 | 16.6 | 41.7 |
| Wide | 22.7 | 21.7 | 44.6 |

These are microbenchmarks, not the browser no-downgrade report. All results
remain observable through a stored output; there is no pass/fail threshold.
The benchmark also prints min/max times and minified browser bundle sizes.
The generated two-function entry point was 840 bytes gzip, including its
needed runtime and `U64` code; the complete experimental runtime was 1,242
bytes gzip. Rerun after changing the mappings or emitter.

## Next boundary

On current main, Charon extracted `moq_net::coding::varint::size` without errors
in about eight seconds including its initial dependency build. The graph had
two functions, four types, and three traits. The emitter refuses its `Form`
and `Result` types, as intended. This is a useful next slice: enum construction
and matching, result values, tuples, calls, and casts between a `U64` and its
halves, before extracting the full codec or session.

The integer experiment shows that the current zigzag source can translate
without rewriting it into halves in Rust. Supporting these bounded operations
in the generic runtime is worth comparing with the quest's current blanket
ban on 64-bit bit operations. Keep that decision open until the codec slice
passes; the production subset and lint have not changed.

Public API impact: none. Wire impact: none. No production code consumes these
prototypes, and the translator quest remains open.
