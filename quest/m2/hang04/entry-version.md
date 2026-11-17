# [M] A player skips a catalog entry newer than it reads

## Goal

A hang catalog entry (a rendition, a data track, an include) can carry its
own `version`: the oldest moq-hang draft that reads that entry correctly. A
reader skips an entry whose `version` is above its own and plays the rest,
and can tell its user that something was skipped. The root `version` stays,
and still refuses the whole catalog, for a change that reshapes the root.
hang-04's one remaining trigger, a `track` that differs from the entry's ID,
moves from the root to the entry. `rs/hang`, `js/hang`, and the hang draft
agree.

## Plan

Decided 2026-11-16 (maintainer, /quest-plan of the pre-cut audit):

- ✅ Skip per entry (rejected: keep the root gate and tell publishers never
  to mix new-style and compatibility renditions in one catalog; keep as
  is). Both hang-04 triggers are per-rendition features, so one such
  rendition takes the whole catalog from an older reader, including the
  compatibility rendition the draft tells publishers to add.

Facts found by the audit:

- A reader "MUST refuse a catalog whose `version` is above the draft it
  implements" (the draft's Version section). `min_version` in
  `rs/hang/src/catalog/root.rs` and `minVersion` in
  `js/hang/src/catalog/root.ts` raise it to 4 for a renamed `track` or a
  video `warmup`, both properties of one entry.
- [moq-dev/moq#5806](https://github.com/moq-dev/moq/pull/5806) saw this
  cost and avoided it for `enabled` ("a live rendition toggle would flip
  the whole catalog unreadable").
- The draft already has the gentler tool: an unknown `container.kind`,
  text `format`, or data `mode` ignores that entry alone.
- Published 0.21 readers check no `version`, so 4 protects nobody deployed
  today. 0.22 is the first reader that checks, and the rule it ships is the
  only forward-compatibility lever every later catalog change gets.

Recommendations for the PR:

- The entry member is named `version`, an integer, absent while it is 3.
  Both serializers compute it from the entry, as the root's is computed
  today, so a caller cannot claim less than the entry uses.
- A reader checks an entry's `version` before it validates the entry, as
  `Probe` reads the root's before any section: a newer entry may have a
  shape this reader would call malformed, and the malformed-rendition rule
  refuses the whole catalog.
- A skipped entry is reported, not silent: the parsed catalog says which
  IDs were skipped and the highest version among them, the per-entry twin
  of `Error::UnsupportedVersion` and JS `UnsupportedVersion`, so a player
  can say "upgrade to see everything".
- With video warmup out of hang-04
  ([moq-dev/moq#8960](https://github.com/moq-dev/moq/pull/8960)), the
  renamed `track` is the only trigger, and nothing in hang-04 raises the
  root `version`. Keep the root check and its tests: it is what a reshaped
  root will use.
- An include is an entry too. Decide whether a skipped include is reported
  the same way, and say so in the draft.
- A reference that an entry makes to a skipped entry (if any section can)
  is the case to look for before settling the rule.

Tests, in both languages and in the parity fixtures: a catalog holding a
compatibility rendition and a version-5 rendition reads as the
compatibility rendition alone and reports the skip; a version-5 entry that
would be malformed to this reader is skipped, not refused; a root
`version` of 5 still refuses; a duplicate `version` member in an entry is
refused as the root's is.

Public API: additive on the parsed catalog (the skipped-entries report);
the computed root `version` changes for a catalog with a renamed `track`.
Wire: the hang-04 catalog JSON, unpublished.

## Related

- [Audio languages](/quest/m3/audio-lang.md) - a later rendition a 0.22 player should skip can be marked with this
