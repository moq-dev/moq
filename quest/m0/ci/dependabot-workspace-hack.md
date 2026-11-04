# [M] Renovate keeps the workspace-hack generated

## Goal

Cargo dependency update PRs arrive with `rs/workspace-hack/Cargo.toml`
matching `cargo hakari generate`, so nobody regenerates it by hand and no
update bumps the hack's transitive pins into duplicate majors. A scheduled,
self-hosted Renovate run from `main` opens them and runs `cargo hakari
generate` after each cargo upgrade. Dependabot stops updating cargo and keeps
every other ecosystem. Pull-request CI stays read-only. Done when a Renovate
cargo group PR passes `cargo hakari generate --diff` (in `just rs lint`)
without a manual commit.

## Plan

✅ Decided 2026-11-04 (maintainer, on
[moq-dev/moq#7819](https://github.com/moq-dev/moq/pull/7819)): measure
hakari's cache benefit and drop it if small. If it is big, keep it and
evaluate Renovate as a protected scheduled updater that runs `cargo hakari
generate`. The benefit measured big, so hakari stays.

**Measurement**, 2026-11-04, on `nixos2` (12 cores, 15 GiB). mbx's memory
budget held it near two parallel compilations, close to a 4-vCPU hosted
runner. Source was [17a81d6c64](https://github.com/moq-dev/moq/commit/17a81d6c64c0ec05e85df3557d688a8963d9f524),
with CI's mbx 1.28.0, a private local store and no remote cache. Each PR
shape is the build CI's Check runs, `sh/rs/select.sh test`, compiled with
`--no-run`, for a one-line `pub const` added to that crate's `lib.rs`.
"Warm" starts from a copy of the store that a cold full-workspace build
left; that build is Nightly's `gate`, which writes the hosted cache. "Cold"
starts empty, as a fork PR without cache credentials does. "No hack" ran
`cargo hakari disable && cargo update --workspace` first.

| PR shape (crates selected) | Warm, hack | Warm, no hack | Cold, hack | Cold, no hack |
| --- | --- | --- | --- | --- |
| moq-relay (2) | 147 s | 288 s | 430 s | 407 s |
| moq-video (7) | 169 s | 310 s | 450 s | 462 s |
| moq-net (30) | 451 s | 610 s | 814 s | 1007 s |
| JS-only | no Rust build | no Rust build | no Rust build | no Rust build |

The hack saves 141-159 s per warm Rust PR build, 26-49% of it. That is
about a fifth of the median hosted Check run: 688 s over the last 40
successful runs. mbx served 725-873 compilations per warm build with the hack
and 425-602 without. Without the hack, 333-396 compilations had no recording
to look up (60-108 with it), because each selection resolves its own
dependency features. Cold builds stay within noise. The moq-net cold gap ran
under higher host load, and its CPU time differs by only 10%: 1663 s
against 1828 s. Hosted R2 hits are partial today (for example, 116 hits and
239 misses with missing-blob warnings in
[run 41789499917](https://github.com/moq-dev/moq/actions/runs/41789499917)),
which [Verify the R2 Rust cache](/quest/m0/ci/mbx-r2-verify.md) tracks. The
self-hosted queue and agents share persistent local stores, which the warm
case models.

**Dependabot cannot do this.** Tested 2026-11-03 in
[moq-dev/moq#7819](https://github.com/moq-dev/moq/pull/7819) by running
Dependabot's own Cargo code in its official updater image, digest
`sha256:a49f8e5edbcbd179325a7cffbde6541cbc17b9ade1d86f61ff8d217b1eca5983`.
Excluding the hack (`exclude-paths`) drops the required manifest, so update
checks fail with `failed to load manifest for dependency workspace-hack`.
Without exclusion it hand-edits the hack's pins, as in
[moq-dev/moq#7782](https://github.com/moq-dev/moq/pull/7782). Ignore rules have
no file scope.

**Renovate**, following hakari's maintainer
([nextest-rs/renovate](https://github.com/nextest-rs/renovate)). The
Mend-hosted app allows only its own undisclosed post-upgrade commands, so
Renovate runs self-hosted:

- A workflow on `schedule` and `workflow_dispatch` runs Renovate against this
  repository. Only that job holds a write credential, through an Environment
  whose branch policy admits only `main`. Use a GitHub App installation token
  with contents and pull-request write, so the PRs it opens trigger CI. The
  maintainer creates the App and the Environment secret; ask them when the
  workflow is ready. Grant no write token to any `pull_request` workflow, and
  never write back to `dependabot/cargo/*`.
- The self-hosted `allowedCommands` admits exactly `^cargo hakari generate$`.
  The cargo manager's `postUpgradeTasks` runs it, with `fileFilters` limited to
  `rs/workspace-hack/Cargo.toml`. `cargo hakari generate` reads `cargo
  metadata` and runs no build script, so the write-token job compiles no
  upgraded code. Install cargo-hakari at the dev shell's version (0.9.39 today)
  where Renovate runs.
- Group every cargo update into one PR, and keep Dependabot's 7-day cooldown
  as `minimumReleaseAge`. Remove the cargo entry from `.github/dependabot.yml`
  in the same PR. Carry that entry's `ignore` caps into `packageRules`, with
  their comments: `uniffi` `allowedVersions: "<0.33"` (uniffi-bindgen-go reads
  one uniffi minor) and `syn` `allowedVersions: "<3"` (kio-macros stays on
  syn 2). Check whether Dependabot security updates for cargo still hand-edit
  the hack; if so, let Renovate's vulnerability alerts take them over.
- Land Renovate's PRs the way Dependabot's land, through `quest merge --land`
  on the pinned head. `.github/workflows/dependabot.yml` requests landing only
  for `dependabot[bot]`, so admit the Renovate App's bot login there too,
  keeping the same-repository head check. Leave Renovate's own `automerge`
  off, since it would bypass that pinned-head request.

Public API: none. Wire: none.

## Related

- [Unify dependency features with cargo-hakari](/quest/m0/ci/hakari.md) - the workspace-hack this keeps generated
- [Read-only pull-request CI](/quest/m0/ci/read-only-secrets.md) - the credential boundary the updater must keep
