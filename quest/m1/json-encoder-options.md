# [S] JSON encoder options without a downcast

## Goal

moq-mux's `json_snapshot` gets its encoder options (after refresh, the
`deltas` switch) without downcasting the config through `Any`. A custom
catalog entry that implements `IntoRendition` can set the same options as
`json::Config`, and the `'static` bound that only served the downcast goes
away.

## Plan

Found by the 2026-10-22 refactor audit (main at d44813cb7):
`delta_ratio_of` in `rs/moq-mux/src/json.rs` recovers the encoder ratio by
downcasting the generic config, and
[moq-dev/moq#6054](https://github.com/moq-dev/moq/pull/6054) renames it to
`deltas_of` but keeps the downcast. `IntoRendition` describes the catalog
entry, so encoder settings that are not catalog fields have no typed path.

Decided 2026-10-22 (maintainer): the PR proposes the API shape once refresh
lands, rather than settling it now. Candidates considered: a separate
options argument (`json_snapshot(track, entry, json::Options)`, recommended
in the interview), or a builder on the returned handle before the first
update. Whichever lands, `json::Config` becomes a catalog entry builder
only, and moq-c's `json_snapshot`, the one in-repo caller that sets
`deltas`, follows. The Impact bullet says how to migrate in moq-mux (and moq-c if its
API changes).

Public API: breaking Rust (moq-mux `json_snapshot` signature, `json::Config`
fields). Wire: none.
