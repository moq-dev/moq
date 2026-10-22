# [M] Playwright browsers from Nix

## Goal

Every Playwright check runs in the root devShell, locally on NixOS and in CI,
against the Chromium build that Nix provides. No check needs a hand-set
`LD_LIBRARY_PATH` or a `playwright install`, and no contributor doc tells
anyone to run one.

Inventory on `main` (2026-10-21). Recheck with `git grep -n -i chromium`
outside `quest/` and `test/smoke/`, keeping the hits that install, download, or
say where Chromium comes from (comments included), and with every
`package.json` that depends on `playwright`:

- Packages: `test/interop/clients/js`, `test/video`, `test/p2p`, `test/wasm`,
  `test/audio-quality/clients/js`, `bench/browser`.
- Scripts: `test/interop/interop.sh`, `test/portrait/run.sh` (run in CI as
  `just test portrait`), `test/wasm/run.sh`,
  `test/audio-quality/{run,record}.sh`, `bench/browser/run.sh`, and the
  `test/justfile` recipes.
- Workflows: `interop.yml`, `video.yml`, `wasm.yml`, `nightly.yml`, including
  their comments that the devShell lacks Chromium.
- Docs and comments: `test/interop/README.md` (says the devShell lacks
  Chromium), `test/wasm/README.md` (says to run
  `bunx playwright install chromium`), `rs/justfile`'s browser clock recipe
  (says `wasm.yml` installs it), `js/justfile`'s fairness sweep ("requires
  installed Chromium"), and `test/video/renderer.browser.ts` (says
  `just test video` installs it).

Out of scope: the Firefox player row, whose `media-firefox` job in
`nightly.yml` keeps installing its engines with `--with-deps`, and
`test/smoke`, which keeps its own flake on purpose.

## Plan

Decided 2026-10-20 with the maintainer:

- **Nix browsers, with npm pinned to match.** Today each package pins npm
  `playwright ^1.64`, and the Chromium it downloads cannot load
  `libglib-2.0.so.0` on NixOS (found while landing the browser rename check).
  The root devShell exports `PLAYWRIGHT_BROWSERS_PATH` from
  `playwright-driver.browsers`, `PLAYWRIGHT_VERSION` from
  `playwright-driver.version`, and `PLAYWRIGHT_SKIP_VALIDATE_HOST_REQUIREMENTS`,
  as `test/smoke/flake.nix` already does; without the last, Playwright's host
  check rejects the Nix browser on NixOS. Every
  package pins npm `playwright` to exactly that version (1.64.1 with the
  current nixpkgs lock), even when this downgrades it. The alternatives were rejected:
  supplying host libraries to a downloaded Chromium (nix-ld or an FHS env)
  depends on the host's library compatibility, and building playwright 1.64
  in the repo's overlay is maintenance for a gap nixpkgs closes on its own.
- **CI switches too.** Every workflow, script, and recipe in the inventory
  drops its Chromium install, so CI and local runs share one Chromium.
- **The pin is enforced.** A check in `just check` fails when any package's
  npm `playwright` differs from `PLAYWRIGHT_VERSION`, like
  `test/smoke/freshness.sh`, so a nixpkgs bump cannot silently drift.
- The docs and comments in the inventory say the devShell provides Chromium.
  No new user-facing docs: this changes only contributor tooling.
