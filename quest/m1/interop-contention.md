# [S] Interop matrices run side by side

## Goal

Two `just test interop --all` matrices can run at once on one machine, even
from one checkout, and both pass. `just test harness` runs from a clean
checkout.

## Plan

[#4228](https://github.com/moq-dev/moq/pull/4228) fixed the port
reservations and the canvas-blocked pause click, and stages Go per run. What
remains:

- Under two concurrent matrices, `python -> js` and `go -> js` sometimes stall
  the browser subscriber: the Pause control never appears within 30 s. It
  was not seen in a single run. Find the cause (CPU starvation of headless
  Chromium, a shared resource, or a real stall) before touching timeouts.
- `prepare_python` runs `just py build` in the workspace, so concurrent runs
  rewrite the same `.venv` and maturin output (Codex on #4228). Build into the
  run directory, as Go now does, or serialize the build.
- `just test harness` runs `harness.browser.ts` without installing workspace
  dependencies or Playwright Chromium, so it only passes where CI's earlier
  step prepared them. Provision them in the recipe, or reuse the path
  `interop.sh` already takes.
- Prove it by running two `--all` matrices at once, several times, from one
  checkout.

Public API: none. Wire: none.
