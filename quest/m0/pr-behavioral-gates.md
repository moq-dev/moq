# [M] Run behavioral gates on the PRs they cover

## Goal

A PR changing media delivery or a binding covered by the existing smoke matrix
(Rust, Python, browser/native JS, C, and GStreamer) gets the applicable
end-to-end check before merge. Expensive gates remain scoped, but a source
change cannot miss its only behavioral test because it did not edit the harness.

## Plan

`.github/workflows/smoke.yml` runs on harness/config changes and nightly, not
ordinary relay, FFI, or browser source PRs. `wasm.yml` covers the WASM dependency
graph but deliberately excludes its JS publisher and relay fixtures. Native
Windows/macOS and broad feature checks live in `nightly.yml`.

The live `main` and `dev` rulesets inspected on 2026-09-05 UTC require `Check`
and `Test` only. A separate Smoke, WASM, or platform result is not a required
context in either ruleset; inspect effective branch protection as well when
wiring the aggregate gate.

Adding Go, Swift, Kotlin, and Dart participants is outside this quest. Report
those bindings as uncovered, and extend the impact map as participants land;
a successful aggregate must not claim behavioral coverage for them.

- Define an explicit impact map in reusable local recipes, extending the
  existing changed-package selection. List what each lane proves and which
  source, build-script, lockfile, feature, or fixture changes select it.
- Run a small representative Rust/browser/C interoperability set on relevant
  source PRs, and the full matrix for wire, FFI, and gateway changes as required
  by Cross-Package Sync. Keep broader combinations nightly when they add cost
  without covering the changed behavior.
- Select native platform and feature compile gates when the changed backend or
  shared build machinery needs them. Keep hardware execution a separate result;
  a platform compile is not a device test.
- Add a stable aggregate result that distinguishes irrelevant from missing,
  failed, cancelled, or timed-out selected jobs. Ensure docs-only PRs complete
  without waiting for a path-filtered check that will never start.
- Audit live branch protection/rulesets before choosing the required result.
  Workflow YAML alone does not establish what GitHub requires for merging.
  Preserve the main-only cache writer policy for new PR lanes.

Acceptance: selector fixtures cover source-only changes in moq-net, moq-ffi,
js/watch, relay, a platform backend, a lockfile, and docs. A deliberately broken
consumer fails the selected gate. Record representative warm/cold costs and
the remaining nightly-only coverage in CONTRIBUTING.

## Related

- [Go smoke client](/quest/m0/smoke-go-client.md) - fills a known missing matrix participant
- [Merge evidence](/quest/m0/merge-verification-evidence.md) - checks freshness and completeness of selected results
- [Runtime QA hosts](/quest/m2/runtime-qa-hosts.md) - supplies execution where hosted compile gates cannot
