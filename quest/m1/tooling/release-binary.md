# [M] One reusable workflow for the binary releases

## Goal

`moq-cli.yml` and `moq-relay.yml` are two copies of the same workflow (226
and 225 lines) with the binary name swapped. They become one reusable
`release-binary.yml` taking `crate` and `bin` inputs, and two callers of a
dozen lines each that keep their tag trigger
and workflow name, so `alert.yml`, `release-brew.yml`, and
`release-winget.yml` keep matching on the names they match today.

## Plan

- `release-binary.yml` on `workflow_call` with inputs `crate` (the cargo
  package) and `bin` (the executable name; `moq-cli` ships `moq`). It holds the Linux native-runner matrix,
  the macOS tarball job, the Windows job, the `.deb` and `.rpm` packaging, the
  release creation, and the repo-publish trigger, all as `just` recipes per
  the preceding quest.
- Each caller keeps `name:` and `on.push.tags` exactly as they are and does
  nothing but `uses: ./.github/workflows/release-binary.yml` with the two
  inputs and the secrets it forwards. `workflow_run` consumers trigger off
  the caller's name, so nothing downstream changes.
- The one text difference between the three today (a comment and a step name
  about the glibc floor) belongs in the reusable file.
- Verify with `just gh check` and by diffing the rendered job list of each
  caller against its predecessor with `gh workflow view`. The next tagged
  release is the end-to-end check; say so in the PR.
