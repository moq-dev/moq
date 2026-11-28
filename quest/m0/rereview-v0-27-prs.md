# [L] Re-review PRs merged under quest v0.27.0-v0.29.0

## Goal

Re-review non-bot PRs merged in `moq-dev/moq` while `quest` ran the flat `0/1/2`
risk-tier review policy (commit `556d816002` / `v0.27.0` through `v0.29.0`,
before `v0.30.0`'s 7-rung `(size + risk)` reviewer ladder from
https://github.com/kixel-ai/quest/pull/957), triage any real bugs still present
on `main`, and fix straightforward ones or file follow-up quests (linking the
originating PR under `## Plan` and recording `## Caused by` in every merged
fixing PR's body).

Under `quest` `v0.27.0`–`v0.29.0`, PRs were under-reviewed relative to the
`v0.30.0` ladder in four ways:

1. `low` PRs received zero local review (`passed with no local reviewer`)
   instead of Sol (`medium`/`high`/`xhigh`, or Sol + Opus on `L`/`XL`).
2. `XS` and `S` `medium` PRs ran `sol("low")` / `sol("medium")` instead of
   `sol("high")` / `sol("xhigh")`.
3. `M`, `L`, and `XL` `medium` PRs ran only 1 seat (Sol alone) instead of 2
   seats (Sol `xhigh` + Opus `medium`/`high`).
4. `XS`, `S`, and `M` `high` PRs ran Sol below `xhigh` (`low`/`medium`/`high`).

Non-goals: re-reviewing `moq-bot` pin bumps, pure `quest/` plan deletions, or
diffs already superseded or reverted on `main`.

## Plan

Decided 2026-11-27 (maintainer): file re-review quests across active Quest
repositories scoped by the `quest` versions used during the `0/1/2` window.

The initial 14 non-quest missed-Sol (`low`) PRs were already re-reviewed and triaged in `moq-dev/moq#9798`.

Repository window:

- `quest` versions observed in `moq-dev/moq`: `556d816002` (#9589), `v0.27.0` (#9615), `v0.28.0` (#9689), `v0.28.1` (#9725), `v0.29.0` (#9782).
- Sole lower bound: PR #9589 (`chore(nix): bump quest to 556d816002`, merged `2026-11-25T07:44Z`).
- Volume snapshot at filing: 144 non-bot PRs merged since #9589.
- The 14 non-quest `passed with no local reviewer` (`low`) PRs merged before `2026-11-27T22:00Z` were already re-reviewed with Sol and triaged in #9798; credit those completed replays and cover every remaining shortfall (including any newer `no-local-review` PRs, zero-review plan additions, under-effort `XS`/`S`/`M` `high` PRs, low-effort `XS`/`S` `medium` PRs, and single-seat `M`/`L`/`XL` `medium` / `L`/`XL` `low` PRs).

Steps:

1. Enumerate every non-bot PR merged in `moq-dev/moq` starting at
   PR #9589 (`chore(nix): bump quest to 556d816002`, merged `2026-11-25T07:44Z`) and continuing through `chore: bump quest to v0.30.0` and reviewer adoption of `v0.30.0` (using actual recorded review runs near both transitions, and recording open PRs at cutover whose `v0.27.0`–`v0.29.0` reviews carry past the pin bump so they are audited on merge unless re-reviewed under `v0.30.0` first).
2. Inspect each PR's `quest review` commit statuses (across all of its
   commits, not only its final head SHA), `## Review` body record, and Kixel
   Quest review runs to compare its actual recorded seats and efforts against
   its `v0.30.0` `(size, risk)` rung:
   - Rung 0 (`XS low`): Sol `medium`
   - Rung 1 (`XS med`, `S low`): Sol `high`
   - Rung 2 (`XS high`, `S med`, `M low`): Sol `xhigh`
   - Rung 3 (`S high`, `M med`, `L low`): Sol `xhigh` + Opus `medium`
   - Rung 4 (`M high`, `L med`, `XL low`): Sol `xhigh` + Opus `high`
   - Rung 5 (`L high`, `XL med`): Sol `xhigh` + Opus `high`
   - Rung 6 (`XL high`): Sol `xhigh` + Opus `xhigh`

   Exclude only `moq-bot` pin bumps, pure `quest/` plan deletions (keep active
   quest/plan additions and edits), diffs already reverted or superseded on
   `main`, and any PR whose shortfall was already covered by an equivalent
   prior replay.
3. Schedule every PR retained by step 2 for the exact missing reviewer seat(s)
   and/or effort upgrade its `v0.30.0` rung requires (read-only against its
   merged diff). Use the following priority order solely to order the queue,
   never to drop a retained PR or lower its required effort:
   - First: under-effort `high`-risk PRs (`XS`/`S`/`M` `high` needing Sol `xhigh`, or any missing second seat).
   - Second: zero-review (`passed with no local reviewer`) PRs.
   - Third: under-effort `XS`/`S` `medium` PRs.
   - Fourth: single-seat `M`/`L`/`XL` `medium` and `L`/`XL` `low` PRs missing their second seat or Sol `xhigh`.
4. Verify every reported `high` or `medium` finding against current `main`,
   drop false positives and already-fixed issues, and either fix small clear
   bugs directly (with tests) or file follow-up quests in `quest/` citing the
   originating PR under `## Plan`. Put `## Caused by` in the body of every
   merged PR that lands a fix (both direct fixes and fixes for follow-up
   quests), where Quest's breakage tracker reads it.

## Waiting

- kixel-ai/quest: `v0.30.0` (the 7-rung reviewer ladder from https://github.com/kixel-ai/quest/pull/957) is published, this repository has merged `chore: bump quest to v0.30.0`, and reviewers have adopted `v0.30.0`. Check `flake.lock` and `gh release list --repo kixel-ai/quest` for `v0.30.0`.
