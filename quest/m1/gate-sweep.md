# [S] Every gate is re-checked on a schedule, so no quest waits on a cleared one

## Goal

A gate is a `Required` bullet in plain text, which `quest guide` defines as
"A plain-text bullet names a condition outside the repository. Periodically
check if it has cleared." Nothing checks, so a gate that clears leaves its
quest blocked forever and the work it describes silently never starts.

After this quest, every quest gated on the outside world states that gate as a
plain-text `Required` bullet, one dated sweep lists every open gate beside its
quest, and a gate whose condition has cleared is promoted to the milestone its
priority belongs in within days rather than never. `quest ready` then tells the
truth about what is blocked.

Boundaries: the sweep does not decide whether a released version carries the
behavior a gate names, and it adds no section to the quest format. A gate whose
condition is a human statement stays a human statement. The sweep reports, and
fails loud only where the tree or the recipe is mechanically wrong; `quest
check` still validates structure and nothing else.

## Plan

What this rests on, all of it re-checkable from the tree:

- 21 of the tree's 719 `Required` entries are plain-text conditions, and 11 of
  them sit in m0..m2. "Re-check the gates periodically" is written twice, in
  the m3 and m4 READMEs, so it reads as those two milestones' private
  convention when it is the whole tree's.
- Two gates have already cleared and nothing noticed. #4428 merged to `dev` on
  2026-09-29 and was the only gate on [quest check
  everywhere](/quest/m0/quest-check-everywhere.md), an m0 quest, so `quest
  ready` still calls it blocked and it is missing from the 225 ready quests.
  #4364 merged to `main` on 2026-09-28 and is one of the two gates on [the
  client-CA quest](/quest/m1/relay-auth-client-ca.md). Both are internal: a
  merge between this repository's own branches, which `git merge-base` alone
  decides.
- The convention is not applied where it matters most.
  `quest/m1/wt-close-upstream.md` has no `## Required` on `main`, so `quest
  ready` calls it ready and lists it among the 225 while its
  `web-transport-moq` release has not shipped (no 1.3.3 tag; moq-dev/noq#21 is
  still open). #4520 adds the bullet. The maintainer decided that quest stays
  in m1 rather than moving to m4, so its gate has to be findable from where it
  is.

Decided:

- **One convention for the whole tree, stated once.** Move the re-check
  sentence into the root [questline](/quest/README.md) Plan and leave each
  milestone README with only what is specific to it. A quest waiting on the
  outside world states its gate as a plain-text `Required` bullet whatever
  milestone it sits in, so `quest ready` blocks it and the sweep finds it.
- **The sweep lists; it does not judge.** A plain-text condition is a sentence
  about the world, and the repository rules say to error on malformed input
  rather than warn and continue. Guessing is the failure: reading "msfts#33
  settles the ES-level payload unit" as "msfts#33 is closed" would promote a
  quest on a condition nobody confirmed. So the sweep prints the gates and
  fails only when the tree is malformed or the recipe cannot run. A deadline on
  a gate is rejected for the mirror reason: a Raspberry Pi in
  [m3](/quest/m3/README.md) can wait two years and still be correct, and age is
  not staleness.
- **The record is a dated comment, not a page and not a failure.** The sweep
  posts one comment per run to one pinned issue, so consecutive comments diff
  to the gate that disappeared and "re-checked on 2026-10-02" is evidence
  rather than a promise. Failing the nightly instead pages Discord every day
  for as long as an m3 hardware gate is open, which teaches people to ignore
  the page. A generated file under `quest/` is not an option: `quest check`
  reads every Markdown document there as a quest and rejects one with no
  `## Goal`.
- **The schedule lives here; the parser does not.** `src/ready.rs` in
  kixelated/quest already models a plain-text bullet as a blocker with no path,
  so a `quest gates` listing there is cleaner and covers every repository
  rather than this one. That is upstream, and the flake input moves when it
  lands. This quest ships the convention and the schedule, which are this
  repository's, and does not wait on the binary: a quest blocked on a release
  of the tool that reports blocked quests is the failure it is about.
- **Promote what has already cleared, first.** Drop the #4428 bullet from
  [quest check everywhere](/quest/m0/quest-check-everywhere.md) and the #4364
  bullet from [the client-CA quest](/quest/m1/relay-auth-client-ca.md), and
  make the sweep name both, so a sweep that finds nothing new is itself
  evidence. Neither quest changes milestone: the cleared gate was never what
  set its rank, and the remaining gate still blocks it.
- **Normalize the m4 file names.** The sweep prints paths, and
  `quest/m4/video-vaapi.md` beside `quest/m4/vaapi-resize-pool.md` gives one
  list two names for the same crate, which is the "cannot tell which is which
  without opening it" cost the m4 README bullets exist to avoid. Rename
  `video-vaapi.md` to `vaapi.md`; `vaapi-resize-pool.md` is already right. No
  PR holds the old `quest/m4/video-vaapi` branch, so the rename costs a branch
  nobody is on.

Out of scope, and why:

- **A quest deleted on `dev` while `main` still lists it.** The repository
  already states the rule ("a quest deleted on `dev` is done, even while `main`
  still lists it"), #4583 is removing the one live instance, and the answer is
  a comparison between this repository's own branches, which `quest`'s
  `overlay` already reads for questlines. It is a different predicate on a
  different cadence, a merge rather than the outside world, and it belongs
  upstream beside `overlay` rather than in a nightly sweep.

## Related

- [Upstream](/quest/m4/README.md) - the milestone whose gates the sweep re-checks, and the only one that says how
- [Deferred](/quest/m3/README.md) - the other milestone with outside-world gates, including the hardware ones a deadline gets wrong
- [quest check everywhere](/quest/m0/quest-check-everywhere.md) - the same validator on more branches; its only gate cleared on 2026-09-29
- [Tooling](/quest/m1/tooling/README.md) - the justfile menu the sweep recipe follows
