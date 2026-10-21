# [M] Playwright browsers from Nix

## Goal

Every Playwright check (`test/interop`, `test/video`, `test/p2p`,
`test/wasm`, `test/audio-quality`, `bench/browser`) runs in the root
devShell, locally on NixOS and in CI, against the Chromium build that Nix
provides. No check needs a hand-set `LD_LIBRARY_PATH` or a
`playwright install`.

Out of scope: the Firefox and WebKit rows, which
[Nightly media players](/quest/m1/media-engines-nightly.md) owns, and
`test/smoke`, which keeps its own flake on purpose.

## Plan

Decided 2026-10-20 with the maintainer:

- **Nix browsers, with npm pinned to match.** Today each package pins npm
  `playwright ^1.64`, and the Chromium it downloads cannot load
  `libglib-2.0.so.0` on NixOS (found while landing the browser rename check).
  The root devShell exports `PLAYWRIGHT_BROWSERS_PATH` from
  `playwright-driver.browsers` and `PLAYWRIGHT_VERSION` from
  `playwright-driver.version`, as `test/smoke/flake.nix` already does. Every
  package pins npm `playwright` to exactly that version, even when this
  downgrades it (1.64 to nixpkgs' 1.63 today). The alternatives were rejected:
  supplying host libraries to a downloaded Chromium (nix-ld or an FHS env)
  depends on the host's library compatibility, and building playwright 1.64
  in the repo's overlay is maintenance for a gap nixpkgs closes on its own.
- **CI switches too.** The video, interop, wasm, and nightly workflows drop
  `playwright install --with-deps` and the `just` recipes drop
  `bunx playwright install chromium`, so CI and local runs share one Chromium.
- **The pin is enforced.** A check in `just check` fails when any package's
  npm `playwright` differs from `PLAYWRIGHT_VERSION`, like
  `test/smoke/freshness.sh`, so a nixpkgs bump cannot silently drift.
- No user-facing docs: this changes only contributor tooling.
