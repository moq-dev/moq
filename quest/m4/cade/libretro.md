# [L] moq-cade runs NES games for several teams through libretro

## Goal

`rs/moq-boy` and `demo/boy` become `rs/moq-cade` and `demo/cade`. The
emulator is a libretro frontend in place of boytacean, it runs an NES core,
and viewers choose a pad under team anarchy. Two browsers on the local demo
that pick different pads control different players in a multiplayer NES game.
Game Boy support, its ROM recipes, and the boytacean dependency are gone.

## Plan

The decisions are in the [epic](/quest/m4/cade/README.md). Recommendations:

- Frontend: load the core with `libloading` and declare the small slice of
  the libretro ABI it uses. `libretro-sys` (2018) and `rust-libretro-sys`
  (2023) are stale, and the slice is short. Answer only the environment
  commands a software core needs. Fail loud on a core that asks for hardware
  rendering, and on a pixel format or command whose answer changes behavior.
  Unsafe code stays in one module, with each FFI invariant documented.
- Core: nixpkgs `libretro.mesen` (GPL-3) or `libretro.nestopia` (GPL-2).
  Measure CPU per instance and pick the cheaper one that handles the chosen
  ROMs. The nix package and the `just` recipes resolve the core path from
  nixpkgs; `--core` takes a path, so nothing searches at runtime.
- Pads: the core's port count, default 2, with a flag for 4 (Four Score) when
  a chosen ROM supports it. Each viewer command carries its pad index, and the
  emulator ORs the buttons of each pad's viewers, as `emulator.rs` does for one
  pad today. Re-picking a pad moves the viewer, and a viewer whose broadcast
  ends leaves its pad, as today. The status track publishes each pad's viewer
  count, so the page shows the teams through MoQ, not by polling.
- Video and audio: encode whatever geometry the core reports and handle a
  geometry change mid-stream (a new catalog rendition or a keyframe at the new
  size). Resample the core's audio rate to the encoder's. NES output is
  256x240; check it against the hardware encoders' minimum sizes and keep the
  `openh264` requirement only if some backend still needs it.
- Rename everything: crate, binary, demo package, element, flake and overlay
  package, the nix.moq.dev cache entry, docs (`doc/bin/demo.md`,
  `doc/setup/examples.md`) with redirects in `doc/worker.ts`, and every quest
  that mentions moq-boy (`grep -ril moq-boy quest doc`). The moq.dev site and
  the demo hosts consume `demo/boy` and the flake's `moq-boy`. If
  [demos-deploy](/quest/m3/release/demos-deploy.md) is still open, update its
  Plan; otherwise list the site and host changes in the PR's Follow-ups for
  their repository.
- Exercise it for real: two browser sessions on `just` in `demo/cade`, one
  per pad, each moving its own player. A test drives a fake core through the
  FFI layer with scripted input and checks per-pad union and geometry changes.

Public API: none (`publish = false`). Wire: none. The viewer command and
status JSON are demo-private.
