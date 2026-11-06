# [S] Playwright bundles the spec exporters

## Goal

The toolchain's Playwright moves to a release whose bundled Chromium is 158
or later and Firefox 160 or later, the first builds whose
`exportKeyingMaterial` follows draft-ietf-webtrans-http3, so the
[browser proof lane](/quest/m2/cluster-routing/routes/browser-proof.md) can
assert both tabs proven.

The root flake's `pkgs.playwright-driver` supplies `PLAYWRIGHT_BROWSERS_PATH`
and `PLAYWRIGHT_VERSION` (`flake.nix`), and the npm pins
(`test/interop/clients/js/package.json`, `bun.lock`) must match it
(`test/smoke/freshness.sh`). As of 2026-11-06 both are 1.64.1, bundling
Chromium 156 and Firefox 157. npm's latest, 1.65.0, bundles Chromium
158.0.8127.18 and Firefox 159.0; no release bundles Firefox 160 yet, and
nixpkgs still carries 1.64.1.

## Plan

Decided 2026-11-06 (maintainer, /quest-complete, on Pullfrog's review of
[moq-dev/moq#8037](https://github.com/moq-dev/moq/pull/8037)): ✅ bump
Playwright, and the lite-07 cut waits for it. Not chosen: running installed
Chrome and Firefox channels instead of the bundled builds; leaving the lane
informational so the cut does not wait.

Take each step as nixpkgs carries it: 1.65.0 or later proves the Chromium
tab, and the first release bundling Firefox 160 or later proves the Firefox
tab. With each bump, move the lane's expectation for that browser from "not
yet covered" or "sends no proof" to "proven". Refresh the flake lock and the
npm pins together, as `smoke-toolchain.yml` describes. Overriding
`playwright-driver` in the flake was considered and not chosen: it would
rebuild and pin browser binaries outside nixpkgs.

## Waiting

- nixpkgs: `playwright-driver` at 1.65.0 or later on `nixpkgs-unstable`;
  check `pkgs/development/web/playwright/driver.nix` there.
- Playwright: a release whose bundled Firefox is 160 or later; check
  `browsers.json` in `npm view playwright-core dist-tags.latest`'s tarball,
  then wait for nixpkgs to carry it.
