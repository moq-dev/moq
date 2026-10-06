# [XS] Cloudflare docs and demo builds track release

## Goal

Condition: the maintainer points the Cloudflare docs (`moq-doc`) and demo
builds' production branch at `release` in the Cloudflare dashboard, as
decided on 2026-10-02 when `main` became trunk and `release` the shipping
branch. Check: the
dashboard's production branch for both projects reads `release`, and the
live docs match `release`, not `main`. Delete this quest once it does.
