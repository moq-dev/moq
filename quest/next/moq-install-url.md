# [S] Publish the moq.dev install URL

## Goal

`curl -fsSL https://moq.dev/install.sh | bash` installs or upgrades `moq`
using the canonical installer from moq-dev/moq. The short URL works without
maintaining another copy of the install logic.

## Plan

- Implement the hosting change in **moq-dev/moq.dev**, which owns the root
  website, not moq.pro. Track this cross-repository work here beside its
  installer dependency; complete this quest only after the website change
  and final documentation update have landed.
- Reuse the site's existing asset/Worker publishing path to expose the
  canonical installer over HTTPS, preferably with a redirect to its published
  source. If using a Worker redirect, include `/install.sh` in
  `assets.run_worker_first` so a missing static asset cannot bypass the route.
  Preserve non-success responses for unavailable scripts rather than serving
  the site's HTML fallback. Keep redirect and cache behavior compatible with
  updates to the canonical installer.
- Verify the route on `new.moq.dev` and test the complete redirected download.
  Run the downloaded script in a temporary install directory and confirm
  `moq --version`. Verify the public URL after an authorized production deploy;
  this plan does not authorize a production deployment.
- Switch `doc/setup/install.md` in moq-dev/moq to the short URL once it is
  working. Keep upgrade and version-selection examples aligned with the
  canonical installer's interface. Add route coverage to the site's normal
  checks for the shell response/redirect and error behavior.

## Required

- [Install moq](/quest/next/moq-installer.md) - canonical installer, release
  selection, upgrade behavior, and tests are published first
