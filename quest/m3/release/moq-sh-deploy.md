# [XS] moq.sh deploys from CI

## Goal

The first `moq.sh` workflow run on `release` deploys the worker with the
`CLOUDFLARE_API_TOKEN` secret. It runs once a release cut brings
`.github/workflows/moq-sh.yml` to `release`, either from the push or from the
dispatch after a `moq-cli` release; `gh workflow run moq-sh.yml --ref release`
starts one by hand. Check with `gh run list --repo moq-dev/moq --workflow
moq-sh.yml --branch release`.

Delete this quest once a deploy job succeeds.

## Plan

The maintainer's manual deploy on 2026-10-05 created the `moq-sh` worker and
its `moq.sh` custom domain; `curl -fsSL https://moq.sh | sh` installs the
newest moq-cli.

Since 2026-11-07 the token holds account Workers Scripts Write and Account
Settings Read, plus Workers Routes Write on the `moq.sh` zone, which
`wrangler deploy` needs because it checks the `moq.sh` custom domain on every
deploy. If the CI deploy is rejected, the error names the missing permission
group: ask the maintainer to add it to the token, then rerun the deploy.

## Required

- [Breaking release cut](/quest/m3/release/cut.md) - prepared code reaches release before artifact publication and installed proof
