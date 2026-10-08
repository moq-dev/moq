# [XS] moq-bot may push workflow changes

## Goal

Condition: the maintainer grants moq-bot's GitHub App the `workflows`
permission, so the Back-merge workflow (`.github/workflows/back-merge.yml`,
which pushes with a moq-bot app token) can push a `release` into `main`
merge that carries changes under `.github/workflows/`. Without it, GitHub
refuses that push and the back-merge stalls until someone merges by hand.

Check: `gh api orgs/moq-dev/installations --jq '.installations[] |
select(.app_slug == "moq-bot") | .permissions.workflows'` prints `write`
(it prints `null` as of 2026-10-08). Advance it by asking the maintainer to
grant it (Settings > GitHub Apps > moq-bot > permissions).

Once it holds, the token step in `back-merge.yml` must also ask for it
(`permission-workflows: write` beside `permission-contents` and
`permission-pull-requests`), since an app token carries only the
permissions it requests. Make that one-line change in the PR that deletes
this quest.

Planned as a follow-up of the 2026-10-05 audit.
