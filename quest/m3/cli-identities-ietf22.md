# [S] moq-cli's ietf22 identities test reads its group

## Goal

`cargo nextest run -p moq-cli --test integration identities::ietf22` passes on every host
`just check` runs on, like its `lite06` and `lite07` siblings.

## Plan

On an Apple Silicon macOS host, it fails deterministically on `origin/main` 93523cf38c (five runs
of five). `recv_group` in `read` (`rs/moq-cli/tests/integration/identities.rs:64`) returns
`Err(Old)`. The merge queue's runs on main pass, so first find out what differs between the hosts:
the platform, or timing between the relay and the group's write. Reproduce it, then fix the root
cause in `moq-net`'s IETF path or in the test, whichever is wrong.

## Hosts

- macos - where it fails
