# [S] Resolve the remaining external Smoke interoperability failures

## Goal

The Smoke workflow passes on main, with every red cell fixed at its cause.
Smoke clients continue to use the interfaces their installed artifacts expose.

## Plan

The Go, C, and Cloudflare clients call their installed interfaces
([moq-dev/moq#6981](https://github.com/moq-dev/moq/pull/6981)), and
`freshness.sh` enforces Go `@latest` and the newest `libmoq`.

What remains is external Docker interoperability. The
[Smoke run](https://github.com/moq-dev/moq/actions/runs/40608138308) builds
the Docker relay, then its second runner matrix fails 14 of 20 cells. Read
the uploaded reports of the latest normal Smoke run and establish each cause
before changing any protocol behavior.
Do not weaken a lane or retry media exchange to make it pass.

Public API: none. Wire: none.
