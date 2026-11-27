# [L] Re-review PRs merged under quest v0.27.0-v0.29.0

## Goal

Re-review non-bot PRs merged in `moq-dev/moq` while `quest` ran the flat `0/1/2`
risk-tier review policy (commit `556d816002` / `v0.27.0` through `v0.29.0`,
before `v0.30.0`'s 7-rung `(size + risk)` reviewer ladder from
https://github.com/kixel-ai/quest/pull/957), triage any real bugs still present
on `main`, and fix straightforward ones or file follow-up quests with
`## Caused by` citing the under-reviewed PR.

Under `quest` `v0.27.0`–`v0.29.0`, PRs were under-reviewed relative to the
`v0.30.0` ladder in four ways:

1. `low` PRs received zero local review (`passed with no local reviewer`)
   instead of Sol (`medium`/`high`/`xhigh`, or Sol + Opus on `L`/`XL`).
2. `XS` and `S` `medium` PRs ran `sol("low")` / `sol("medium")` instead of
   `sol("high")` / `sol("xhigh")`.
3. `M`, `L`, and `XL` `medium` PRs ran only 1 seat (Sol alone) instead of 2
   seats (Sol `xhigh` + Opus `medium`/`high`).
4. `S` and `M` `high` PRs ran below Sol `xhigh`.

Non-goals: re-reviewing `moq-bot` pin bumps, pure `quest/` plan deletions, or
diffs already superseded or reverted on `main`.

## Plan

Decided 2026-11-27 (maintainer): file re-review quests across active Quest
repositories scoped by the `quest` versions used during the `0/1/2` window.

The initial 14 non-quest missed-Sol (`low`) PRs were already re-reviewed and triaged in `moq-dev/moq#9798`.

Repository window:

- `quest` versions observed in `moq-dev/moq`: `556d816002` (#9589), `v0.27.0` (#9615), `v0.28.0` (#9689), `v0.28.1` (#9725), `v0.29.0` (#9782) (window starts at
  `2026-11-25T07:34:04Z` when `kixel-ai/quest#882` merged and ends when this
  repository moves past `v0.29.0` to `v0.30.0`).
- Volume snapshot at filing: 144 non-bot PRs merged since `556d816002` (99 code/config/docs, 45 quest-titled).
- The 14 non-quest `passed with no local reviewer` (`low`) PRs merged before 2026-11-27T22:00Z were already re-reviewed with Sol and filed/fixed in #9798; focus this pass on any newer `no-local-review` PRs plus under-effort `XS`/`S` (`sol("low")`/`sol("medium")`) and single-seat `M`/`L`/`XL` `medium` PRs.

Steps:

1. Enumerate every non-bot PR merged in `moq-dev/moq` between
   `2026-11-25T07:34:04Z` (or the repository's `v0.27.0`/`556d816002` bump)
   and the commit that bumps `quest` to `v0.30.0`.
2. Inspect each PR's `quest review` commit statuses (across its commits, not
   only its final head SHA), `## Review` body record, and Kixel Quest review
   runs to classify what review it actually received (`no-local-review`,
   `sol("low")`/`sol("medium")`, single-seat `M`/`L`/`XL` `medium`, or
   under-effort `high`). Drop PRs whose touched lines have already been
   reverted or superseded on `main`, plus pure `quest/` plan deletions and
   bot pin bumps.
3. Replay missing reviews read-only against each remaining PR's merged diff in
   priority order:
   - First: zero-review (`passed with no local reviewer`) non-plan PRs with
     Sol at their `v0.30.0` rung effort (plus Opus on `L`/`XL`).
   - Second: `XS`/`S` code/config PRs reviewed at `sol("low")` or
     `sol("medium")` with Sol `high`/`xhigh`.
   - Third: `M`/`L`/`XL` `medium` code/config PRs missing their second seat
     (Opus `medium`/`high`) or Sol `xhigh`.
4. Verify every reported `high` or `medium` finding against current `main`,
   drop false positives and already-fixed issues, and either fix small clear
   bugs directly (with tests) or file follow-up quests in `quest/` citing the
   originating PR in `## Caused by`.

## Waiting

- kixel-ai/quest: `v0.30.0` (the 7-rung reviewer ladder from https://github.com/kixel-ai/quest/pull/957) is released and this repository has merged `chore: bump quest to v0.30.0`, closing the `v0.27.0`–`v0.29.0` window. Check `flake.lock` / `gh release list --repo kixel-ai/quest` for `v0.30.0`.
