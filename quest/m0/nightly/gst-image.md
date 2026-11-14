# [M] Publish a GStreamer CI image to ghcr.io every week

## Goal

A weekly job on `main` publishes a multi-arch (amd64, arm64) image to ghcr.io
with Ubuntu 24.04's GStreamer packages preinstalled, and proposes bumping the
digest pin whenever the image changes. moq-gst's Linux jobs can then pull it
instead of fetching from Ubuntu's apt mirrors, as
[moq-gst's apt outage](/quest/m0/nightly/release-builds.md) plans.

## Plan

Decided 2026-11-13 (maintainer, /quest-complete): ✅ a weekly-published
ghcr.io image with the packages preinstalled (rejected: accepting a mirror
outage as a real failure). This quest publishes the image; the consumer
switch is its own quest, since a pin needs a digest that only a `main` run
can publish.

- The Dockerfile lives in `.github/docker/gstreamer/`. Pin its `ubuntu:24.04`
  base by digest, as the root Dockerfile does, and add the directory to
  Dependabot's `docker` ecosystem. Install what both consumers need:
  `gstreamer1.0-tools`, `gstreamer1.0-plugins-base` and
  `gstreamer1.0-plugins-good` for the smoke test, plus
  `libgstreamer1.0-dev`, `libgstreamer-plugins-base1.0-dev`, `pkgconf` and
  `gcc`/`libc6-dev` for the package build. Label it
  `org.opencontainers.image.source=https://github.com/moq-dev/moq` so the
  package links to the repository and this repo's Actions can pull it with
  `GITHUB_TOKEN`.
- Follow `docker.yml`'s shape: build linux/amd64 on `ubuntu-latest` and
  linux/arm64 on `ubuntu-24.04-arm` natively, push by digest, and assemble
  the manifest list in a merge job. Name it `ghcr.io/moq-dev/gstreamer-ci`.
  Triggers: a weekly `schedule`, `workflow_dispatch`, and `push` to `main`
  touching the Dockerfile. Only jobs in the `publish` environment (which
  deploys from `main`, `release` and release tags only) get
  `packages: write`, so a quest branch's run can't push.
- Dependabot can't bump a `container:` or `docker run` reference in a
  workflow
  ([dependabot/dependabot-core#5819](https://github.com/dependabot/dependabot-core/issues/5819)),
  so the publisher proposes the bump itself through moq-bot, the way
  `smoke-toolchain.yml` does. Keep the pin in a plain file,
  `.github/docker/gstreamer/image` (one `ghcr.io/...@sha256:...` line), that
  the consumers read, not in a workflow file: an app token that edits
  workflows needs `workflows: write`.
- A scheduled run skips the push and the PR when the built image's
  `dpkg-query -W` package list matches the pinned image's, so a week with no
  Ubuntu updates opens no PR. A push or dispatch always publishes, so a
  Dockerfile change that installs the same packages still ships. With no pin
  file yet, the first run publishes and proposes the file.
- Validate the image locally with podman before pushing: the smoke test
  loads a moq-gst tarball, and `cargo zigbuild` of `moq-gst` links in it.
  After merge, confirm the push-triggered run published both architectures
  and opened the pin PR.
