# [M] Codec refactors for rs2ts

## Goal

moq-net's lite codec (`coding/` encode, decode, varint, and version, plus
the `lite/` message types) can be extracted without pulling in the model,
the session, or async. Every tracing call in moq-net goes through a
crate-local facade. The codec does no u128 math. Native behavior, wire, and
codec benchmarks are unchanged.

## Plan

Decided 2026-10-11: the codec stage's refactors are planned now, the model
stage's later in [Generated model](/quest/m2/rs2ts-model.md). This waits on
[the spike](/quest/m1/rs2ts/spike.md), so the chosen foundation's actual
gaps shape it.

- Move `Hop`, `Hops`, and `Cost` (encoded at `model/origin.rs:176-340`) and
  `track::Position` (`model/subscription.rs`) into a small value module, so
  the codec does not drag in the 10k-line `origin.rs`. Keep their public
  paths through re-exports.
- Split the non-codec parts out of codec files: `ProbeLevel::detect` and
  `PeerSetup` (a `kio::Shared`) in `lite/setup.rs`. `coding/{reader,writer,stream}.rs`
  go with the session, not the codec.
- Tracing facade: about 87 non-test calls, 3 on the hot encode/decode path
  in `lite/message.rs`. Natively the facade expands to `tracing`, unchanged.
  Under the cfg rs2ts extracts with, it calls a plain function that the
  translator binds to hand-written TS that logs to the console (decided:
  keep logging in generated code), so no fmt or callsite machinery is
  translated. Convert all of moq-net at once; it is mechanical.
- Replace the codec's u128 uses (`Duration::as_millis`/`as_nanos` in
  `lite/auth.rs:106`, `lite/track.rs:103-105`, `lite/subscribe.rs:96`) with
  checked u64 math that keeps the same results and errors. The same goes
  for `Timescale::new` (`model/time.rs`), which TRACK_INFO decoding calls and
  which widens to u128 for its `2^62 - 1` bound; check the bound in u64.
- Keep zigzag and the u64 halves as they are: the runtime's `U64` provides
  the bit operations (the #5049 prototype translated zigzag exactly).
- Benchmark `--bench codec` before and after.

Public API: none intended; the moved types keep their paths. Wire: none.

## Required

- [rust-js spike](/quest/m1/rs2ts/spike.md) - the chosen foundation's gaps decide the final list
