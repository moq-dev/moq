# [S] Playwright bundles Chromium's spec exporter

## Goal

The toolchain's Playwright moves to a release whose bundled Chromium is 158
or later, the first build whose `exportKeyingMaterial` follows
draft-ietf-webtrans-http3, so the route key lane
(`test/interop/clients/js/route-key.ts`) proves the Chromium tab. The lane
already requires a proof from Chromium 158 on, so the bump changes no
expectation. Firefox's exporter departs from the draft in every build so far;
[its own quest](/quest/m2/cluster-routing/routes/browser-proof.md) carries
that bump.

The root flake's `pkgs.playwright-driver` supplies `PLAYWRIGHT_BROWSERS_PATH`
and `PLAYWRIGHT_VERSION` (`flake.nix`), and the npm pins
(`test/interop/clients/js/package.json`, `bun.lock`) must match it
(`test/smoke/freshness.sh`). As of 2026-11-06 both are 1.64.1, bundling
Chromium 156 and Firefox 157. Playwright 1.65.0 (npm, and playwright-python
since 2026-11-09) bundles Chromium 158.0.8127.18 and Firefox 159.0; no
release bundles Firefox 160 yet. As of 2026-11-09 nixpkgs still carries
1.64.1, with its bump open.

## Plan

Decided 2026-11-06 (maintainer, /quest-complete, on Pullfrog's review of
[moq-dev/moq#8037](https://github.com/moq-dev/moq/pull/8037)): ✅ bump
Playwright, and the lite-07 cut waits for it. Not chosen: running installed
Chrome and Firefox channels instead of the bundled builds; leaving the lane
informational so the cut does not wait.

Take the bump once nixpkgs carries 1.65.0 or later, and check that the lane
passes the Chromium tab. Refresh the flake lock and the npm pins together, as
`smoke-toolchain.yml` describes. Overriding
`playwright-driver` in the flake was considered and not chosen: it would
rebuild and pin browser binaries outside nixpkgs.

## Waiting

- nixpkgs: [NixOS/nixpkgs#585789](https://github.com/NixOS/nixpkgs/pull/585789)
  (`playwright-driver` 1.64.1 -> 1.65.0, from the kixelated/nixpkgs fork)
  merges and reaches `nixpkgs-unstable`; check that
  `pkgs/development/web/playwright/driver.nix` on `nixpkgs-unstable` reads
  1.65.0 or later. Answer nixpkgs reviewers on that PR to advance it.
