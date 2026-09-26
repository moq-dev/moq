# [S] Demo recipes call scripts

## Goal

The demo justfiles hold the last inline bash in the `just` tree. Move each
recipe body with real logic into a script the recipe calls, so every recipe
is a thin command sequence:

- `demo/pub`: `hls` (a background ffmpeg, playlist polling, and a cleanup
  trap) and `serve-hls`.
- `demo/justfile`: `default` (port probing and the `concurrently` launch),
  `port`, and `wait`.
- `demo/relay`: `ca`, `cert`, `key`, and `token`, the idempotent certificate
  and token generation.
- `demo/pub` and `demo/boy`: `sync`, the same R2 upload loop with a different
  bucket, which becomes one script taking the bucket. `download` in both, and
  the argument check in `demo/pub`'s `clock`, go with them.

Behavior stays the same, including Windows Git Bash, where `port` falls back
without `lsof`.

## Plan

- Put the scripts under `sh/demo/`, matching the `sh/<module>/` layout of the
  other modules, and run `shellcheck` and `shfmt` on them through the existing
  `shell` lint.
- Short command sequences (`just rs features`, `just py check`, the `ffmpeg`
  pipelines) are thin already and stay inline.

Public API: none. Wire: none.
