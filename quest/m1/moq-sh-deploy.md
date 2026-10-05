# [XS] Deploy moq.sh

## Goal

`curl -fsSL https://moq.sh | sh` installs `moq`, and CI keeps it deployed.

1. Once the installer is on `main`, run `just infra moq-sh deploy` by hand
   under the maintainer's `wrangler login`. This creates the worker and its
   `moq.sh` custom domain, which the CI token cannot do. Then run
   `curl -fsSL https://moq.sh | sh -s -- --dir "$(mktemp -d)"` and check the
   installed `moq --version`.
2. The first `moq.sh` workflow run on `release` deploys with the
   `CLOUDFLARE_API_TOKEN` secret. It runs once a release cut brings
   `.github/workflows/moq-sh.yml` to `release`, either from the push or from
   the dispatch after a `moq-cli` release; `gh workflow run moq-sh.yml --ref
   release` starts one by hand. Check with `gh run list --repo moq-dev/moq
   --workflow moq-sh.yml --branch release`.

Delete this quest once both are done.

## Plan

The token has the account-wide Workers Editor role and no Zone access.
Cloudflare says custom domains do not support per-Worker roles yet, and
`wrangler deploy` checks the `moq.sh` custom domain on every deploy. If the CI
deploy is rejected on the custom domain, ask the maintainer to add
Zone > Workers Routes > Edit on `moq.sh` to the token, then rerun it.
