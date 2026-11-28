# [M] Store Path without unsafe

## Goal

`moq_net::Path` holds its bytes in a representation built from safe Rust,
such as `&'static str` and `Arc<str>`, and no function in
`rs/moq-net/src/path/` uses `unsafe`. Its public API, wire format, and
behavior stay the same. Done when the change lands with its performance cost
measured against moq's benches, and rust2-lean translates `Path`'s functions
through its `str` models instead of the hand-written `String` model it carries
today. Also fixes `rs/moq-net/verify/README.md`, which says 656 route-algebra
results are checked by Agda where the generators write 649.

## Plan

Why: rust2-lean (a translator from Rust to Lean 4, in kixel-ai/rust2) checks
moq-net's route algebra in Lean. It can't translate `Path`'s inline storage,
so it models `Path` by hand as a Lean `String`
([kixel-ai/rust2#1071](https://github.com/kixel-ai/rust2/pull/1071),
[lean-router](https://github.com/kixel-ai/rust2/blob/main/quest/m2/lean-router.md)).
A hand model can drift from the code it stands for.

Facts from `main` at 22e3f0ad89:

- `Repr` is `Inline { len: u8, buf: [u8; 22] }`, `Static(&'static str)`, or
  `Shared { start: u16, buf: Arc<str> }`, 24 bytes on 64-bit targets
  ([moq-dev/moq#6843](https://github.com/moq-dev/moq/pull/6843)). Its only
  `unsafe` is `from_utf8_unchecked` on the inline bytes in `Path::as_str`.
  `Path::concat` and `ietf::namespace::from_tuple` also build paths in
  `arrayvec` stack buffers, a dependency whose types rust2-lean has no model
  for.
- #6843's allocation counts (`rs/moq-net/benches/path.rs`, deterministic)
  show what the inline form saves: one allocation per short or medium
  `Path::new`, two to three per short `join`, and about one per decoded or
  looked-up short path. Announce fan-out did not change. Times were not
  measured beyond a spike that found faster short `clone`, `new`, and `join`.
- `rs/moq-net/verify/route-algebra/README.md` already says 649
  (`Vectors.agda` 329, `ClaimVectors.agda` 320).

Decided 2026-11-27 (maintainer, via
[kixel-ai/rust2#1071](https://github.com/kixel-ai/rust2/pull/1071)): the
long-term goal is no hand-written models in rust2-lean, and moq may change to
get there. ✅ Replace `Path`'s unsafe storage with a safe representation
(rejected: keep the Lean model indefinitely). ✅ Fold the 656-to-649 fix into
this quest (rejected: its own quest; drop). Weigh the safe form's performance
cost, which this quest measures, against moq's benches.

Decided 2026-11-28 (maintainer, /quest-run in kixel-ai/rust2,
[moq-dev/moq#9841](https://github.com/moq-dev/moq/pull/9841)), accepting each
recommendation the headless `/quest-plan` of 2026-11-27 made:

- ✅ Try `Static` plus `Shared` alone first, dropping `Inline` (rejected:
  starting with a checked inline form, `str::from_utf8(..).expect(..)` on
  every `as_str`, which validates on each read and needs rust2-lean models
  for byte arrays and `from_utf8`; a small-string crate such as `smol_str`
  or `compact_str`, which carry their own `unsafe` and so fix nothing for
  rust2-lean). `Arc<str>`, `&'static str`, and slicing already translate.
- ✅ Measure before choosing, against `main`: the allocation rows of
  `benches/path.rs`; `just bench-fanout origin/main`'s instruction counts;
  and moq-net's `origin`, `announce`, and `retirement` benches. Micro rows will
  allocate more by design; the end-to-end rows decide. If the `Shared`-only
  form moves an end-to-end row by more than an A/A run of the same bench
  does, measure the checked inline form too and take the cheaper. If both
  do, don't land: put both sets of numbers to the maintainer as a `Waiting`
  decision between the cheaper safe form and keeping today's storage and the
  Lean model (rejected: a fixed regression budget, which would be an
  arbitrary bar).
- ✅ The quest doesn't need `arrayvec` gone from the stack buffers unless
  rust2-lean reaches them; record which `Path` functions its census still
  refuses (rejected: requiring every builder to drop `arrayvec`, which costs
  an allocation per long path for functions the route algebra may never
  call).
- ✅ In `verify/README.md`, drop the count and point at
  `route-algebra/README.md`, which holds the breakdown (rejected: change 656
  to 649, which drifts again the next time the generators change).
- ✅ m3, since a named consumer (rust2-lean's `lean-router` quest) retires
  its model once this lands (rejected: m1, which ranks ahead of the release
  work though nothing is blocked, since rust2-lean carries its model
  meanwhile; m4, which is for work no one waits on).

Validation: `just ci`; the bench table above in the PR; and rust2-lean
translating `Path` from this branch's Rust. In a kixel-ai/rust2 checkout with
the hand-written model (`driver/lean/src/lower/path.rs`, which overrides
`as_str`, `empty`, and `head` whatever `Path` holds) removed, run
`just driver lean --census` on this branch's `rs/moq-net/Cargo.toml` from
`Path::as_str`, `Path::empty`, and `Path::head`: all three translate, with no
refusal from `Path`'s representation. `just driver lean-moq` alone shows
nothing here, since it pins an older moq revision and keeps the model.

## Related

- delete moq-pattern - its PR, [moq-dev/moq#9800](https://github.com/moq-dev/moq/pull/9800), moves prefixes into `rs/moq-net/src/path/`; whichever lands second rebases
