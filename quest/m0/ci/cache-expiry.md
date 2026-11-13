# [XS] Supplemental caches expire sooner

## Goal

`mbx-moq-dev` deletes objects 7 days after upload, and `nix-ci-moq-dev` after
14 days. `refresh`'s cutoff fits the new Nix window. Both rules are applied to
the buckets, and no doc, comment or quest still names the old windows.

## Plan

Decided 2026-11-13 (maintainer, /quest-plan): ✅ shorten mbx from 14 to 7 days
(`infra/mbx/lifecycle.json`), Nix CI from 30 to 14 days
(`infra/nix/lifecycle.json`), and fit `refresh`'s 7-day cutoff in
`infra/nix/justfile` to the new window. Both caches are supplemental. A missing mbx
object is a rebuild, and a narinfo whose NAR expired makes Nix fall back to
building. The projected saving is about $7 a month. Rejected: keeping today's
windows, and R2 Infrequent Access. mbx serves 2-5 TB of reads a day, so
retrieval fees alone would be $20-50 a day.

Measured 2026-11-13: `mbx-moq-dev` held 998 GB in 501k objects. Its 14-day
expiry was applied 2026-11-07. `nix-ci-moq-dev` held 48.8 GB and grows about
1.5 GB a day.

`refresh` re-copies every object in the newest cached commit's closures once
it is older than the cutoff, which restarts its age. A path survives as long
as some `main` push refreshes it between the cutoff and the expiry. A shorter
cutoff widens that window but copies hot objects more often, and each copy is
a Class A request: copies a month scale as 30 divided by the cutoff in days.
The Nix storage saved by the 14-day expiry is under $1 a month, so extra
copies can cost more than it saves. Recommended: keep the 7-day cutoff, which
under a 14-day expiry still leaves 7 days for a push to `main`. Shorten it
only if measured closures expire between pushes, after counting the closure's
objects and the copy requests a shorter cutoff adds.

Apply both rules with `just infra mbx lifecycle` and `just infra nix
lifecycle`, using the maintainer's wrangler login as
[mbx-r2-verify](/quest/m0/ci/mbx-r2-verify.md) does. The maintainer already
chose these values. Record the date and each bucket's `wrangler r2 bucket
lifecycle list` output in the PR.

Update the text that names the old windows: `infra/README.md`, the
30-day expiry in `flake.nix`'s comments and in `.github/workflows/smoke.yml`'s
comment on its pinned builds, the comment on
`refresh`, mbx-r2-verify's Goal and its 2026-11-26 measurement, which then
splits at this change too, and
[cachix-retire](/quest/m0/ci/cachix-retire.md)'s 30-day references if that
quest is still open.

Public API: none. Wire: none.

## Related

- [Verify the R2 Rust cache](/quest/m0/ci/mbx-r2-verify.md) - its 2026-11-26 measurement counts the rebuilds the expiry causes and can argue for lengthening it again
- [The Rust cache stops paying for uploads R2 refuses](/quest/m0/ci/mbx-failed-puts.md) - the larger, request side of the same bill
