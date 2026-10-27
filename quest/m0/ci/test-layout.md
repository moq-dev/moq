# [L] Measure moving unit tests out of hub crates

## Goal

Decide from measured CI CPU time whether hub crates, starting with moq-net,
should move unit tests into external harnesses (`[lib] test = false`) so the
library stops compiling a second time as the unit-test target. Until that
evidence exists, moq keeps its inline unit tests.

## Plan

Decided with the maintainer on 2026-10-25: measure before restructuring.

Deprioritized 2026-10-26: a first pass found no evidence of savings and a
large migration cost, so keep the inline layout. Load-bearing facts, at main
5516a406f4:

- About half of moq-net's source is test code, and its library harness has
  about 2.7k tests. moq-mux has a similar share.
- Roughly 1k of those tests name private items. Moving them needs a
  test-support surface that does not exist. The `fuzz` feature is not one: it
  is public and implies `async`, which would break the no-default-feature
  boundary.
- A four-case prototype worked mechanically but measured nothing: the shared
  host was loaded and cache state differed between runs.
- Interop, bare-FIN, the feed/fetch benchmark, and `sh/rs/net-features.sh`
  select library tests, so a migration must carry them too.

Revisit only if a profile, after the thin-generics and BTreeMap work, shows the
unit-test target's library build is still a large share of a moq-net edit's
CPU. A credible comparison then needs a settled test-support design (or
private-state tests kept inline), a prototype that preserves every compiled test
across feature sets, and identical edit-plus-tests runs with mbx warm on a quiet
host.

Public API: none, unless the test-support surface leaks; say so if it does.
