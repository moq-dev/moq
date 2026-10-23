# [XS] Remove the actionlint queue-key workaround

## Goal

`.github/actionlint.yaml` drops its ignore for the `queue` key once an
actionlint release after 1.7.12 is in the dev shell.

## Plan

[rhysd/actionlint#661](https://github.com/rhysd/actionlint/pull/661) is merged
but unreleased. The dev shell still has actionlint 1.7.12.
[moq-dev/moq#6180](https://github.com/moq-dev/moq/pull/6180) added the ignore
for the three Pages publishing workflows; remove it when the release reaches
nixpkgs and `just gh check` accepts `queue: max`.

Public API: none. Wire: none.

## Related

- [CI workflow audit](/quest/m2/audit-ci.md) - the broader per-job review
