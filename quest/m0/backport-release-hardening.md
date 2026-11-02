# [S] Backport the dot-segment and auth-redirect refusals to release

## Goal

The `release` branch's relay refuses connection paths with `.` or `..` segments before asking the auth server, and `release`'s auth client refuses redirects from the auth server, matching main. The fixes ship in the next patch release a human cuts.

## Plan

Decided 2026-10-30 (maintainer, follow-up planning after the /quest-afk run): combine these backports into one later patch release, with versions chosen by a human.

- The main prerequisite [moq-dev/moq#7366](https://github.com/moq-dev/moq/pull/7366) merged on 2026-11-01. The hand-port of its dot-segment refusal and [moq-dev/moq#6384](https://github.com/moq-dev/moq/pull/6384)'s auth-client redirect refusal is [moq-dev/moq#7599](https://github.com/moq-dev/moq/pull/7599), targeting `release`.
- That backport remains draft: full local CI and the io_uring compile cannot launch Cargo because of the existing mbx `E2BIG` defect tracked in [moq-dev/moq#7594](https://github.com/moq-dev/moq/pull/7594). Resume those checks after the infrastructure fix. The selected Rust package checks, all 588 selected tests, API compatibility checks, and other-vendor security review pass; the code PR records the evidence.
- Once validated and approved, land the code PR on `release` and include it in the planned combined patch release. No versions, tags, publication, deployment, or merges were authorized for this implementation task.
- Keep this main checkpoint draft until that patch publishes, then delete the quest and its milestone link, following [moq-dev/moq#7381](https://github.com/moq-dev/moq/pull/7381). The earlier [moq-dev/moq#7389](https://github.com/moq-dev/moq/pull/7389) backport is already merged and remains a separate scope.

Public API: none. Wire format: none; the backport changes admission and auth HTTP refusal behavior.

## Related

- [Backport the SETUP-path and iroh-key fixes](/quest/m0/backport-security-fixes.md) - the earlier backport in the same patch release
