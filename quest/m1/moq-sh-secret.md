# [XS] Cloudflare deploy secret for moq.sh

## Goal

A maintainer adds a `CLOUDFLARE_API_TOKEN` Actions secret to moq-dev/moq,
scoped to Workers Scripts: Edit on account `dd618f5dbd5da77b8296f1613c301f5c`
and Zone access for `moq.sh`, so the release workflow from
[the moq.sh installer](/quest/m1/moq-sh.md) can deploy its worker.

Check with `gh secret list --repo moq-dev/moq`. Delete this quest once the
secret exists and a `release` push has deployed the worker.

## Related

- [moq.sh installer](/quest/m1/moq-sh.md) - the workflow that uses the secret
