# [XS] select.sh resolves its launcher before cd

## Goal

`cd sh/rs && bash select.sh lint --all` runs the selected checks instead of
failing with `bash: ./../local.sh: No such file or directory`.

## Plan

[moq-dev/moq#6256](https://github.com/moq-dev/moq/pull/6256) re-execs
`sh/rs/select.sh` through `sh/local.sh` after changing to the repository
root, so a relative `$0` no longer resolves
([review thread](https://github.com/moq-dev/moq/pull/6256#discussion_r4366860705)).
Resolve both paths before the `cd`, or name them relative to the root.

Public API: none. Wire: none.
