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

## Waiting

- Maintainer: create the `go-moq-dev` bucket and deploy the Worker
  (`just infra go create`, `just infra go deploy`), and give the `publish`
  Environment's R2 token Object Read & Write on the bucket; steps 1 and 2 of
  "Bootstrapping the Go module proxy" in `infra/README.md`. An agent holds no
  Cloudflare login or R2 write key. Done when `curl -fsS https://go.moq.dev/`
  prints the banner.
- Maintainer: backfill and verify with that token
  (`just infra go backfill`, then `just infra go verify`), step 3. Done when
  `verify` prints a go.sum line for each of the 45 or more versions.
- Maintainer: cut moq.dev over, step 4: in moq-dev/moq.dev, `worker/vanity.ts`
  serves `mod https://go.moq.dev` for `/moq-ffi` without a `go-source` tag
  (that repository's quest tree owns the change; a tested patch is in
  [moq-dev/moq#8743](https://github.com/moq-dev/moq/pull/8743)'s Follow-ups),
  and its deploy needs the maintainer's permission. Then run step 5 (`just infra go backfill`,
  `just infra go verify --proxy direct`,
  `just infra go verify --proxy https://proxy.golang.org`). Done when
  `just go ffi-origin` prints `mod` and both verifies pass.
- Maintainer: once the next `moq-ffi` release publishes through go.moq.dev
  (its `Publish to go.moq.dev` step succeeds), archive the mirror with
  `gh repo archive moq-dev/moq-go-ffi --yes`, step 6. Done when
  `gh api repos/moq-dev/moq-go-ffi --jq .archived` prints `true`.

## Related

- [Go per-platform lib modules](/quest/m3/go-ffi-platform-modules.md) - splits the staticlibs per platform, out of this quest
