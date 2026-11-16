# [S] The drafts directory holds only drafts the code implements

## Goal

`drafts/` contains no document that describes nothing in the repository.
`draft-lcurley-moq-epoch.md` and `draft-lcurley-moq-use-cases.md` are
deleted, `draft-lcurley-moq-probe.md` is deleted or reduced to a pointer at
the lite draft's Probe section, and no draft, doc page, or quest links to
them. `drafts/README.md`'s claim that each draft is "implemented by the Rust
and JS code elsewhere in this repo" is then true.

## Plan

Decided 2026-11-16 (maintainer, /quest-plan of the pre-cut audit):

- ✅ Delete the dead drafts (rejected: keep them in a `drafts/proposals/`
  directory that the doc site does not render as specifications). An
  announcement points readers at `/draft/`, and each of these says it was
  generated from an implementation that does not exist.

Facts found by the audit (datatracker state queried 2026-11-15):

- **epoch** (204 lines, never published): implemented 2026-10-19 and
  removed 2026-11-01 in
  [moq-dev/moq#7239](https://github.com/moq-dev/moq/pull/7239) because "no
  third party implements it". Keeping the draft "as an unimplemented
  proposal" was the implementing agent's choice against a reviewer, not a
  recorded decision. Its EPOCH is what lite-07 calls an Instance, so the
  name now misleads. `drafts/draft-lcurley-moq-cluster.md` leans on it in
  five places (`grep -n -i epoch`), and `doc/concept/standard.md` links it.
- **use-cases** (443 lines, -01 published): no commit in the window. It
  names "moq-karp", a `timeline` track, and aligning tracks by Group
  Sequence, all gone.
- **probe** (220 lines, -00 published): no code sends or reads its stream
  type (`grep -rni 0x950BE rs js` is empty), and
  `js/net/src/ietf/connection.ts` says "moq-transport has no PROBE".
  lite-07's own PROBE uses bits per second and RTT, not this draft's
  kilobits and elapsed time. `doc/concept/standard.md` lists it among the
  drafts that "extend the IETF wire" and then describes lite behaviour.
  `quest/m5/probe-padding.md` tells its PR to update the draft.

For the PR:

- Recommended for probe: delete it. A published -00 stays on the
  datatracker and expires on its own; a body that only points elsewhere is
  not a draft. Keep the pointer form only if something outside the
  repository links `/draft/moq-probe`.
- Delete the files, then follow `grep -rn 'moq-epoch\|moq-probe\|use-cases' drafts doc quest`:
  the five epoch passages in the cluster draft, the two links in
  `doc/concept/standard.md` (rewrite that paragraph to say what Rust
  sessions do on lite, and that moq-transport sessions mint an instance per
  announcement, with no proposal behind it), and the probe sentence in
  `quest/m5/probe-padding.md`. The doc site's draft list is generated from
  `drafts/` (`doc/.vitepress/drafts.ts`), so it follows.
- The cluster draft is not this quest's: the
  [route switch](/quest/m2/multicast/switch.md) retires it. If that lands
  first, the five epoch passages are already gone.
- While in `drafts/`: the `ietf` and `hidden` drafts each carry a
  "Since ...-00 (in progress)" changelog heading for a -00 that was never
  published. `drafts/AGENTS.md` says "never add a section for an unpublished
  next version"; remove both headings if that rule covers a first version.

Run `just drafts check`.

Public API: none. Wire: none. Docs: three `/draft/` pages disappear.

## Related

- [The route switch](/quest/m2/multicast/switch.md) - retires the moq-cluster draft that cites epoch
