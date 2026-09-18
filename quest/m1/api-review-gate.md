# [XS] The API review is landed or deferred, quest by quest

## Goal

Every `api-*` quest in [m1](/quest/m1/README.md) from the 2026-09-18
dev-to-main review has a recorded outcome before the merge PR opens: landed
on dev, or deferred by the maintainer with the semver cost accepted. The
readiness tooling reads `Required`, not prose, so this gate is what keeps
[Merge dev](/quest/m1/merge-dev.md) from reporting ready while breaking
reviews are unresolved.

## Plan

The maintainer walks the list and, per quest, either lets it land (the quest
file is deleted on completion) or deletes the quest with a note in
`quest/m1/README.md` naming the deferred break. When the list is empty this
quest is deleted too. No code.

The list: [Auth contract](/quest/m1/auth-contract.md),
[Announce event](/quest/m1/api-net-announce.md),
[Origin scoping](/quest/m1/api-net-origin.md),
[moq-tokio shapes](/quest/m1/api-tokio-shapes.md),
[@moq/net API](/quest/m1/api-js-net.md),
[@moq/auth API](/quest/m1/api-js-auth.md),
[JSON configs](/quest/m1/api-json-config.md),
[Catalog types](/quest/m1/api-hang-catalog.md),
[Rendition ownership](/quest/m1/api-mux-rendition.md),
[Watch and publish shapes](/quest/m1/api-watch-publish.md),
[Gateway types](/quest/m1/api-gateways.md),
[libmoq units](/quest/m1/api-libmoq-units.md).

## Related

- [Merge dev](/quest/m1/merge-dev.md) - requires this gate
