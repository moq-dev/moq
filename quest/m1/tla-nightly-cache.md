# [XS] Nightly re-checks a TLA+ config only when its inputs change

## Goal

Nightly runs each exhaustive or fixed-seed TLA+ config only when its spec,
config, or TLC version changed since its last pass, and keeps running the
unseeded simulation configs every night.

## Plan

Found 2026-11-14 by the [feature review](/quest/m3/release/feature-review.md).
`sh/tla.sh` reads only the spec, the config, and TLC, and 26 of the 30 configs
are exhaustive or pin `-seed 1`, so they give the same result on unchanged
inputs. On 2026-11-14 the TLA+ legs were Nightly's three longest jobs (102,
98, and 74 min) and the route leg set its end; over 14 nights their only
failures were CI configuration, such as a deleted config, never a broken
model. Key a cache of passing results on a hash of spec, config, and TLC
version (`actions/cache` works), and run every config whenever the key
misses.

Decided 2026-11-14 (maintainer, pick on [moq-dev/moq#8764](https://github.com/moq-dev/moq/pull/8764)): ✅ (a) cache passing results by input hash (rejected: (b) every config every night).

Public API: none. Wire: none.

