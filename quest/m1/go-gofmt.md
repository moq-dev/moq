# [XS] The Go wrapper stays gofmt-clean

## Goal

`go/wrapper` is gofmt-clean, and `just go check` fails on unformatted Go, so
CI catches formatting drift.

## Plan

Found while landing [moq-dev/moq#7044](https://github.com/moq-dev/moq/pull/7044)
(planned 2026-10-28): `gofmt -l` flags `go/wrapper/client.go`,
`errors_test.go`, `audio/audio.go`, and `video/video.go`, and
`sh/go/check.sh` runs `go build`, `go vet`, and `go test` but never gofmt.
Format those files, then add a `gofmt -l` step to `sh/go/check.sh` that fails
when it prints anything. Cover generated Go only if the generator's output is
meant to be gofmt-clean.

Public API: none. Wire: none.
