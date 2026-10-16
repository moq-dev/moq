# [XS] Require the platform gates on release

## Goal

Condition: a cut carries #5488's `.github/workflows/platform.yml` to
`release`. Check with
`git diff origin/main origin/release -- .github/workflows/platform.yml`;
until the `Platform scope` job and the `base_ref == 'release'` gate appear on
`release`, release PRs run the old path-filtered workflow, which never reports
`Platform scope` and skips unrelated diffs, so requiring the contexts would
block backports forever.

Once it clears, a maintainer adds `Platform scope`, `macOS`, and `Windows`
from the GitHub Actions integration (15368) to release ruleset 24392632,
keeping `Check` and `Test`. Confirm the next release-targeting PR runs the
selected platform jobs and reports all three contexts.
