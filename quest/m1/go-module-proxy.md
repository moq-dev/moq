# [S] Go module proxy

## Goal

`moq.dev/moq-ffi` is served from a static GOPROXY that we host, so its
staticlibs never enter git. `go get moq.dev/moq-ffi` keeps working with no
extra step, through proxy.golang.org and with `GOPROXY=direct`. Every
published version, old and new, verifies against sum.golang.org. The
`moq-dev/moq-go-ffi` mirror stops receiving releases and is archived.

Today each release commits about 61 MiB of compressed staticlibs to that
mirror. It reached 610 MB after 35 releases in 12 weeks, and about 819 MiB
with 44 tags on 2026-11-12 (`gh api repos/moq-dev/moq-go-ffi --jq .size`,
in KiB), so it passes GitHub's 1 GB guidance in about three more releases.

## Plan

The proxy is built: the `infra/go` Worker serves `go.moq.dev` out of the
`go-moq-dev` bucket, `sh/go/publish-ffi.sh` publishes each release there, and
`just infra go backfill` and `just infra go verify` copy and check every
earlier version. Until moq.dev's go-import tag says `mod https://go.moq.dev`,
the release job reads that tag (`sh/go/ffi-origin.sh`) and keeps publishing
to the git mirror, so releases keep working while the maintainer bootstraps.

Once the mirror is archived, delete the transition:

- `sh/go/publish-ffi-mirror.sh`, `sh/go/ffi-origin.sh`, and their
  `publish-ffi-mirror` and `ffi-origin` recipes in `go/justfile`.
- The `git` steps of the `publish` job in `release-go-ffi.yml`, which then
  needs no moq-bot token, and the `git` case of the wrapper gate in
  `release-go.yml`, which then queries `go.moq.dev` alone.
- `sh/go/package-ffi.sh`'s GitHub 100 MiB size check and its
  `--skip-size-check` flag, which `sh/go/stage.sh` passes. `sh/go/modzip`
  enforces the module zip's own limits.
- The "until the cutover" text in `go/ffi/README.md` and `infra/README.md`.

Steps 1 to 3 ran on 2026-11-21 (maintainer-approved, from the maintainer's
desktop): the `go-moq-dev` bucket and Worker are live, `https://go.moq.dev/`
prints the banner, and `backfill` then `verify` covered all 46 versions. Those
two ran with a temporary token scoped to `go-moq-dev`, since deleted. The
`publish` Environment's R2 secret was matched to the "MoQ CI" token by date
(both 2026-11-05; the only other apt/rpm token dates from May), and that token
now has Object Read & Write on `go-moq-dev`. No run has used the stored secret
against the bucket yet: the first is the next release's `Publish to go.moq.dev`
step, which step 6 already waits on.

Steps 4 and 5 ran on 2026-11-21: moq-dev/moq.dev#281 merged and its main
Worker deployed, so `https://moq.dev/moq-ffi?go-get=1` serves
`mod https://go.moq.dev` and `just go ffi-origin` prints `mod`. The catch-up
backfill had nothing to copy (proxy.golang.org and go.moq.dev both list the
same 46 versions), and `verify --proxy direct` and
`verify --proxy https://proxy.golang.org` each printed 46 go.sum lines.

## Waiting

- Maintainer: once the next `moq-ffi` release publishes through go.moq.dev
  (its `Publish to go.moq.dev` step succeeds), archive the mirror with
  `gh repo archive moq-dev/moq-go-ffi --yes`, step 6. Done when
  `gh api repos/moq-dev/moq-go-ffi --jq .archived` prints `true`.

## Related

- [Go per-platform lib modules](/quest/m3/go-ffi-platform-modules.md) - splits the staticlibs per platform, out of this quest
