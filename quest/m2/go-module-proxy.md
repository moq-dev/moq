# [L] Go module proxy

## Goal

`moq.dev/moq-ffi` is served from a static GOPROXY that we host, so its
staticlibs never enter git. `go get moq.dev/moq-ffi` keeps working with no
extra step, through proxy.golang.org and with `GOPROXY=direct`. Every
published version, old and new, verifies against sum.golang.org. The
`moq-dev/moq-go-ffi` mirror stops receiving releases and is archived.

Today each release commits about 61 MiB of compressed staticlibs to that
mirror. It reached 610 MB after 35 releases in 12 weeks, so it passes
GitHub's 1 GB guidance within weeks.

## Plan

Decided 2026-10-17 in the [Go mirror delivery](https://github.com/moq-dev/moq/pull/5853)
evaluation: our own proxy at `go.moq.dev`, run from `infra/go`. It rejected
the alternatives:

- Orphan or squashed mirror history still stores every tagged release, unless
  old tags are deleted, and deleting them breaks `GOPROXY=direct`.
- Per-platform git mirrors split the growth without stopping it.
- Release assets fetched by a build step break plain `go get`.

The go command follows a `go-import` tag whose VCS is `mod` to any
GOPROXY-protocol URL. `riverqueue.com/riverui` ships built assets this way,
and proxy.golang.org caches it like any other module.

- A Worker in `infra/go` serves `go.moq.dev` read-only out of a `go-moq-dev`
  R2 bucket, like `infra/apt`: `<module>/@v/list`, `<version>.info`, `.mod`,
  and `.zip`. A 404 is final: nothing falls back past this proxy, even with
  `GOPROXY=direct`. Versioned files are immutable, so cache them forever and
  keep `list` short-lived.
- Build module zips with `golang.org/x/mod/zip` from a small in-tree Go tool,
  not a hand-rolled `zip`, since the go command validates the layout.
- `publish-ffi.sh` uploads with rclone and the existing R2 secrets. It refuses
  to overwrite an existing version, because sum.golang.org pins the first
  hash it sees. It uploads `list` last, so a version is listed only once its
  `.info`, `.mod`, and `.zip` are served.
- Backfill every version in proxy.golang.org's `list` by copying
  `go mod download`'s `cache/download` files, which are already the GOPROXY
  layout. Identical contents keep the h1 hashes. Check each version against
  sum.golang.org with `GOPROXY=https://go.moq.dev` before cutover.
- Cutover is one table entry in `moq-dev/moq.dev`'s `worker/vanity.ts`: emit
  `mod https://go.moq.dev` for `/moq-ffi` and drop its `go-source` tag, since
  the generated `moq.go` lives in no repo.
- Retarget the wrapper gate in `release-go.yml` before archiving: it looks
  for the FFI tag and `go.mod` on the `moq-go-ffi` mirror, so a wrapper-only
  release would defer forever. Query `moq.dev/moq-ffi/@v/<version>.mod` on
  `go.moq.dev` instead, failing loudly on anything but a 200 or 404.
- Verify from a clean module cache with both `GOPROXY=direct` and the default
  proxy, then archive `moq-dev/moq-go-ffi` without deleting it.
- Decided: the `moq.dev/moq` wrapper is pure Go and stays on its git mirror.
- Update `go/ffi/README.md`, `doc/lib/go`, and the `infra/README.md` table.
- Keep the per-platform split out of this quest; it lands in
  [Go per-platform lib modules](/quest/m2/go-ffi-platform-modules.md).
