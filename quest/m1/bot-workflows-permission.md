# [XS] moq-bot may push workflow changes

## Goal

Condition: the maintainer grants moq-bot's GitHub App the `workflows`
permission, so the Back-merge workflow (`.github/workflows/back-merge.yml`,
which pushes with a moq-bot app token) can push a `release` into `main`
merge that carries changes under `.github/workflows/`. Without it, GitHub
refuses that push and the back-merge stalls until someone merges by hand.

Check: the app's installation permissions on moq-dev/moq list Workflows as
read and write (Settings > GitHub Apps > moq-bot > permissions, or
`gh api /repos/moq-dev/moq/installation` with an app JWT). Delete this quest
once it does.

Planned as a follow-up of the 2026-10-05 audit.
