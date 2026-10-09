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

The token has the account-wide Workers Editor role and no Zone access.
Cloudflare says custom domains do not support per-Worker roles yet, and
`wrangler deploy` checks the `moq.sh` custom domain on every deploy. If the CI
deploy is rejected on the custom domain, ask the maintainer to add
Zone > Workers Routes > Edit on `moq.sh` to the token, then rerun it.
