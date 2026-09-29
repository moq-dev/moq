# [S] Slim Docker images

## Goal

Each published image contains only its package's nix closure. Today the
final stage of `Dockerfile` is `nixos/nix:latest`, which puts about 170 MiB
of nix runtime into every image. `moqdev/moq-relay` is about 186 MiB, and
`moqdev/moq-token-cli` is 172 MiB for a 1.6 MiB binary.

## Plan

Decided in planning: use a `scratch` final stage holding the closure and the
binary, with no shell. Losing `docker exec ... sh` debugging is accepted.

Guidance:

- `entry.sh` needs `/bin/sh`, and exec-form `ENTRYPOINT` doesn't expand build
  args. In the builder stage, create a fixed-name symlink to the package's
  binary and point `ENTRYPOINT` at it.
- Check what the runtime needs outside the closure: CA roots for outbound
  TLS (cluster peers, auth fetches), `/tmp`, and a non-root user if the
  current image has one. The package's closure should carry them; verify
  outbound TLS works rather than assume it.
- Decide what happens to the no-package `sh` default, which only makes
  sense with a shell.
- Report before and after image sizes for each published image, and update
  any docs that `docker exec` into a shell.
