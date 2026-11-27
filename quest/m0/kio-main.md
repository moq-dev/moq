# [M] Move moq to kio's main and keep moq-transport relay ingest to one pacing window

## Goal

`moq` pins `kixel-ai/kio` at a `main` revision carrying the adversarial scheduler ([kixel-ai/kio#10](https://github.com/kixel-ai/kio/pull/10)) and the MoQ-name removal ([kixel-ai/kio#22](https://github.com/kixel-ai/kio/pull/22)), the content of kio's coming `0.7.0`, with every check passing. In particular `fastest_reader::two_relays_bound_their_cache_and_resume_without_a_hole` passes on `moq-transport-22` and `-23` without loosening its bound: a relay's ingest holds at most one pacing window of unread payload on a transport that does not shape credit.

## Plan

Found 2026-11-27 while starting [kio-crates-io](/quest/m0/kio-crates-io.md) ([moq-dev/moq#9749](https://github.com/moq-dev/moq/pull/9749)) by bumping the pin from `866ef062` to kio `main` at `073df54a`.

The regression, reproduced deterministically (six seeds, all failing identically):

- `cargo nextest run -p moq-net --test integration --all-features -E 'test(two_relays_bound_their_cache)'` fails with `moq-transport-22: hop 2 retained 2036464 bytes` (the bound is `MB + FRAME`, 1114112).
- Bisected on kio: `1303c7d9` passes, `048ead4b` ([kixel-ai/kio#10](https://github.com/kixel-ai/kio/pull/10), "adversarial schedules and a lost-wakeup probe") fails. Its default `fifo` schedule changed: an explicit yield now goes behind the ready queue, and `Tasks` passes report sim events.
- Lite is unaffected: hop 2 holds about 1 MiB on both kio revisions. On `moq-transport-22` and `-23`, hop 2 holds 986272 B on the old pin and 2036464 B on the new one; hop 1 grows by the same 1 MiB of sent history, and the origin writes 343 frames instead of 327.
- One group stream per hop on both versions (counted with the mock transport's `uni_writes`). The downstream stream carried 3080383 B on IETF against 2097316 B on lite, so hop 2's IETF ingest read about two windows (2 MiB) into the group while its only reader had taken nothing, with the third MiB in stream credit. The mock transport implements no `set_window`, so [`model/pace.rs`](/rs/moq-net/src/model/pace.rs)'s "twice that on a transport that shapes" allowance does not apply: this breaks the one-window contract, not a test assumption.
- Start at the IETF subscriber's group ingest: how it reads `Pause::window` and parks against `Pace`, compared with lite's. The `verify-credit` TLA+ model covers pacing and paused budgets; extend it if the cause is an ordering the model lacks.

The migration to kio `main` itself is mechanical (done and reverted on [moq-dev/moq#9749](https://github.com/moq-dev/moq/pull/9749); there `just check` passed every module but the Rust tests, whose run stopped at this failure with about 4,200 tests not yet run):

- Pin `073df54a` (or newer) in `Cargo.toml`, `bench/fanout/{session,model,group}/Cargo.toml`, and `rs/workspace-hack/Cargo.toml` (`cargo hakari generate` then adds `futures`, since kio now builds it without default features).
- Edit the three lockfiles' kio `source` lines and point `kio-macros` at the `syn 3` already locked (`3.0.6` in `Cargo.lock`, `3.0.10` in `bench/fanout/session/` and `demo/carla/bridge/`). `cargo update -p kio` also reshuffles unrelated `windows-sys`/`socket2` edges; `cargo metadata --locked` accepts the hand edit.
- Drop the stale kio entries from `nix/crate-hashes.json`, then `just rs cargo-nix`.
- `Wall::MOQ_EPOCH` is gone: add `pub const EPOCH: kio::clock::Wall = kio::clock::Wall::from_unix_secs(1_577_836_800)` to `moq_net::time` and use it in `rs/hang`, `rs/moq-mux`, `rs/moq-gst`, `rs/moq-hls`, `rs/moq-net`, `rs/moq-rtsp`, and `rs/moq-stats` (`grep -rn MOQ_EPOCH rs --include=*.rs`). Add a `moq-net` `MIGRATE.md` note for the new export.
- `MOQ_SIM_SEED` is now `KIO_SIM_SEED`: update `doc/lib/rs/kio.md`.
- `kio::Tasks` grows a `Vec` under `sim`, pushing `moq-relay`'s `DialersPhase::Open` past clippy's `large_enum_variant`: box it (`Open(Box<DialersState>)`). Prefer the box to an `#[expect]`, which could go unfulfilled in builds without `sim`, where the variant is 24 bytes smaller.

Public API: `moq_net::time::EPOCH` is added. Wire: none.
