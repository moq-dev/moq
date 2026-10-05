# [XS] Cloudflare deploy secret for moq.sh

## Goal

A maintainer adds a `CLOUDFLARE_API_TOKEN` Actions secret to moq-dev/moq,
granting the Workers **Editor** role scoped to the `moq-sh` worker on account
`dd618f5dbd5da77b8296f1613c301f5c`, so the release workflow from
[the moq.sh installer](/quest/m1/moq-sh.md) can deploy it.

Check with `gh secret list --repo moq-dev/moq`. Delete this quest once the
secret exists and a `release` push has deployed the worker.

## Plan

- Do not use the legacy Workers Scripts permissions; Editor replaces
  Workers Scripts Edit.
- The installer quest's manual deploy creates the worker and its `moq.sh`
  custom domain. After that, Editor can deploy new versions as long as a
  deploy does not add, change, or remove a route or custom domain, so CI
  gets no Zone > Workers Routes permission. A config change that touches
  the domain is deployed by hand.

## Required

- [moq.sh installer](/quest/m1/moq-sh.md) - creates the worker the token
  is scoped to
