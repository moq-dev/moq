# [M] The Rust cache stops paying for uploads R2 refuses

## Goal

`mbx-moq-dev` takes measurably fewer Class A requests per scheduled night,
mostly by failing fewer conditional puts with HTTP 412. Each hosted Rust-cache
job keeps its hit rate within its night-to-night spread. A root fix is
proposed upstream in jdx/mr-boxington so that mbx stops issuing puts it can
tell will fail.

## Plan

Measured 2026-11-13 in a read-only Cloudflare audit of Oct 10 to Nov 9. R2
cost $38.35 (Class A $22.50, Class B $8.64, storage $7.21), almost all of it
`mbx-moq-dev` (998 GB, 501k objects). That bucket served 5.65M Class A
requests and 24.9M Class B. 4.80M of the Class A requests failed with HTTP
412. mbx uploads each output with a conditional put (`If-None-Match`), and 85%
of those puts fail because the object already exists. R2 still bills each one
as Class A, about $21 a month. The audit window straddles the 30 → 14-day
expiry change of 2026-11-07, and a shorter expiry removes some 412s: an
expired object's put succeeds instead, as a paid upload. So size the
workaround and the upstream fix from nights after 2026-11-07 only, as
[mbx-r2-verify](/quest/m0/ci/mbx-r2-verify.md) splits its measurement. A
shorter expiry still leaves every repeat upload of an object that is alive.

The audit named the writers as the hosted jobs on `main`: Nightly, CodSpeed,
and `platform.yml` on push. Checked against `main` on 2026-11-13,
`platform.yml` runs on push only for `release`, where the jobs only read,
since `.github/actions/rust-cache` writes only on `refs/heads/main`. On `main`
it runs only inside Nightly. So every writer is scheduled or dispatched:
Nightly and the workflows it calls, plus CodSpeed, all at 07:00 UTC, then
`interop.yml` and `ts.yml` at 08:00. Jobs that share a runner namespace start
together, so the likely cause is several jobs compiling the same crates at
once, with each one uploading them. This is unconfirmed. Other possible causes
are mbx re-uploading outputs it restored or prefetched, or several keys
sharing one stored object. Find out which from mbx's upload path and from
hourly request counts before you pick the writer set, since the cause decides
the fix.

Decided 2026-11-13 (maintainer, /quest-plan):

- ✅ A workaround here: fewer jobs write the remote cache. The decision named
  writing only from Nightly instead of every push to `main`. Since no push
  writes now, choose whichever writer set cuts the 412s and measurably keeps
  hit rates. Candidates are one writer per runner namespace and workload,
  staggered schedules, or read-only CodSpeed, interop and ts jobs.
  Recommended: add a write opt-in input to the rust-cache action, set it only
  on the chosen jobs, and drop the `cache-write` Environment from the jobs
  that only read, so fewer jobs hold the write key. Update the action's
  "Hosted runs on `main` write" comment, `infra/README.md`'s "Hosted jobs on
  `main` write what they compile", and the
  [CI epic](/quest/m0/ci/README.md)'s record of who writes.
- ✅ The root fix belongs upstream in mbx: before the conditional put, check
  whether the object exists with a HEAD (Class B), or skip outputs mbx already
  knows are stored. jdx/mr-boxington is outside the maintainer's orgs. Its
  CONTRIBUTING.md, read 2026-11-13, allows AI-assisted Discussions, Issues and
  pull requests that say AI contributed. It sends proposed changes to
  Discussions first and closes bug reports that came from reading the source
  rather than from using mbx, so report from these measurements. Re-read it
  and its AGENTS.md before drafting. If they now ban LLM-written posts, draft
  nothing and say so in the PR. Otherwise draft the post (or the pull
  request) in this quest's PR body and add a `Maintainer:` item under
  Waiting to approve posting it. An agent could post it, but the maintainer
  approves each post outside their orgs.
- Rejected: only shortening the expiry, which leaves the failed writes alone
  (shortened to 7 days on 2026-11-12 for storage).

Measure before and after over enough scheduled nights to separate the change
from the spread between nights:

- Class A requests by action and response status, and 412s, per hour. Use
  Cloudflare's GraphQL `r2OperationsAdaptiveGroups`, filtered on `bucketName`,
  with the maintainer's wrangler login as
  [mbx-r2-verify](/quest/m0/ci/mbx-r2-verify.md) does.
- Hits, misses and not looked up for each Rust-cache job, from its mbx cache
  summary.

An expired object turns a 412 into a successful upload. Compare nights under
one lifecycle window, or note the
2026-11-12 expiry change between them.

If the PR lands before upstream fixes this, rewrite this quest to what is
left: the posting approval and the upstream fix. Plan the mbx bump that adopts
the fix as its own quest. That quest re-measures, and if the workaround cost
hits, it restores the writers it dropped.

Public API: none. Wire: none.

## Related

- [Verify the R2 Rust cache](/quest/m0/ci/mbx-r2-verify.md) - its 2026-11-26 bucket measurement and its Windows report to mbx overlap this one's numbers and upstream contact
