# [M] Playwright browsers from Nix

## Goal

Every Playwright check runs in the root devShell, locally on NixOS and in CI,
against the Chromium build that Nix provides. No check needs a hand-set
`LD_LIBRARY_PATH` or a `playwright install`, and no contributor doc tells
anyone to run one.

Inventory on `main` (2026-10-21); recheck with
`git grep -n "playwright install"` and every `package.json` that depends on
`playwright`:

- Packages: `test/interop/clients/js`, `test/video`, `test/p2p`, `test/wasm`,
  `test/audio-quality/clients/js`, `bench/browser`.
- Scripts: `test/interop/interop.sh`, `test/portrait/run.sh` (run in CI as
  `just test portrait`), `test/audio-quality/{run,record}.sh`,
  `bench/browser/run.sh`, and the `test/justfile` recipes.
- Workflows: `interop.yml`, `video.yml`, `wasm.yml`, `nightly.yml`.
- Docs: `test/interop/README.md` (says the devShell lacks Chromium) and
  `test/wasm/README.md` (says to run `bunx playwright install chromium`).

Out of scope: the Firefox and WebKit rows, which
[Nightly media players](/quest/m1/media-engines-nightly.md) owns (its
`nightly.yml` step keeps installing those engines), and `test/smoke`, which
keeps its own flake on purpose.

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
- **CI switches too.** Every workflow, script, and recipe in the inventory
  drops its Chromium install, so CI and local runs share one Chromium.
- **The pin is enforced.** A check in `just check` fails when any package's
  npm `playwright` differs from `PLAYWRIGHT_VERSION`, like
  `test/smoke/freshness.sh`, so a nixpkgs bump cannot silently drift.
- The READMEs in the inventory say the devShell provides Chromium. No new
  user-facing docs: this changes only contributor tooling.
