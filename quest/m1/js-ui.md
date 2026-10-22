# [M] Shared watch and publish UI

## Goal

`@moq/watch` and `@moq/publish` import their identical presentation code
from a new `@moq/ui` package instead of each carrying a copy: the stats
graph, the button and settings button components, and the support element's
banner and details rendering. Capability detection stays in each product.

## Plan

Found by the 2026-10-22 refactor audit (main at d44813cb7): `ui/graph.ts`
(152 lines), `ui/components/button.ts` and `ui/components/settings-button.ts`
are byte-identical across `js/watch` and `js/publish`, and
`support/element.ts` differs in about 74 of its roughly 300 lines.

Decided 2026-10-22 (maintainer): a narrow published `@moq/ui` package over
an `@moq/signals` subpath (media widgets would blur a generic library) or
publish depending on watch (wrong relationship). It holds presentation only.
Move other shared helpers only when identical and used by both.

Validate both packages' independent published builds, DOM cleanup, and the
demo. The package gets a README; add it wherever the JS packages are listed
for publishing and docs.

Public API: new `@moq/ui` package; `@moq/watch` and `@moq/publish` exports
unchanged. Wire: none.
