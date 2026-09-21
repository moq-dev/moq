# [XS] The API review is landed or deferred, quest by quest

## Goal

Every `api-*` quest in [dev](/quest/dev/README.md) from the 2026-09-18
dev-to-main review has a recorded outcome before the merge PR opens: landed
on dev, or deferred by the maintainer with the semver cost accepted. The
readiness tooling reads `Required`, not prose, so this gate is what keeps
[Merge dev](/quest/dev/merge-dev.md) from reporting ready while breaking
reviews are unresolved.

## Plan

The maintainer walks the list and, per quest, either lets it land (the quest
file is deleted on completion) or deletes the quest with a note in
`quest/dev/README.md` naming the deferred break. When the list is empty this
quest is deleted too. No code.

The list: [Rendition ownership](/quest/dev/api-mux-rendition.md).

## Related

- [Merge dev](/quest/dev/merge-dev.md) - requires this gate
