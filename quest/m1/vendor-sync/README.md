# Vendored code has every upstream fix it needs

## Goal

Once per release cycle, every library vendored into moq-dev/moq, plus
moq-dev/vaapi and the UniFFI generator forks, is compared against its upstream
from the last checked commit. Useful fixes and features are ported or planned,
each library records the newest upstream commit checked, and code vendored
since the last run gets its own child.

## Plan

Decided 2026-10-30 (maintainer). moq vendors upstream code in-tree rather than
keeping forks in separate repositories, and cargo-deny cannot see an advisory
in vendored code, so checking upstream is recurring work. One questline covers
it, with one child per upstream, so the children run in parallel.

Each child, each run:

- Compare upstream's default branch, plus its security advisories, against the
  last checked commit its Plan names.
- Port an XS security or correctness fix with its regression test in the
  child's PR, or record why it does not apply. Plan anything larger, and any
  upstream feature worth having, with `/quest-plan` as a follow-up. Skip
  dependency bumps.
- Record the newest upstream commit checked where the next run will look: an
  `Upstream` section in the vendored crate's README (or module doc), or the
  child's Plan for code outside this repository.

This README's own work, once every child has merged:

1. Scan the tree for code vendored since the last run: new crates or modules
   whose README, Cargo description, or module doc says forked, vendored, or
   ported from an upstream, and any new `git` dependency or flake input
   pointing at a fork. Each one gets a child in the next run, starting from its
   vendored commit. Code whose upstream is gone or frozen (moq-tokio's
   `quic::web`, from moq-dev/noq) is listed in the README it lives in and
   gets no child.
2. Re-create the questline at `/quest/m2/vendor-sync/`, linked from
   `/quest/m2/README.md`'s `Required`. Every child keeps its Goal and carries
   forward its last checked commit and what this run learned (what to check
   first, what was ruled out). Each child requires
   [the release cut](/quest/m2/release/cut.md), which removes itself from
   every `Required` and shifts m2 into m1, so the next run becomes ready in the
   new m1, once per release cycle.
3. Drop a child whose code was deleted or whose upstream stopped mattering,
   saying why in the PR.

Only `/quest-delete` ends the recurrence.

Out of scope: forks moq no longer consumes (moq-dev/quiche, moq-dev/noq,
kixelated/cros-codecs, kixelated/openh264-rs, kixelated/fflate) and the
`@kixelated/libavjs-webcodecs-polyfill` npm fork js/hang uses (maintainer,
2026-10-30: its upstream has not changed since 2025-07).

Public API: none expected; a ported fix that changes one says so. Wire: QUIC
behavior follows quinn's fixes.

Completed VAAPI cycle (2026-11-04): source fix and read-only CI merged in
[moq-dev/vaapi#5](https://github.com/moq-dev/vaapi/pull/5) and
[moq-dev/vaapi#6](https://github.com/moq-dev/vaapi/pull/6). MoQ adopts crates.io
`moq-vaapi 0.1.1` (tag `v0.1.1`, source `af121c75ccd63d98205e50e4561f88cce8bdf82b`);
the published manifest no longer enables `log/release_max_level_debug`.
Carry the following baseline and dispositions into the next cycle.

moq-vaapi vendors Discord's forks: cros-libva tag `discord-0.0.13`
([`896a248e`](https://github.com/discord/cros-libva/commit/896a248e003caae7a7120812f7eaa423c93a920d),
2026-03-13) and cros-codecs tag `discord-0.0.5`
([`9cdecb19`](https://github.com/discord/cros-codecs/commit/9cdecb192ee3682950d02bd80a7f106163531b76),
2026-06-15). Track the ChromeOS default branches against these diverged fork
baselines, without undoing Discord's additions.

Checked 2026-10-31:

- cros-libva
  [`59384456ac2ae78c0c3e5515f41ef1efd9b802cf`](https://github.com/chromeos/cros-libva/commit/59384456ac2ae78c0c3e5515f41ef1efd9b802cf):
  all five commits absent from the vendor baseline inspected. Explicit image
  lifetimes and packed headers are already present. The retained VP9 wrapper
  already zero-fills the libva 2.23 fields with `..Default::default()`; there
  is no VP9 codec driver needing segmentation control. Protected-content
  headers have no consumer. Borrowed input buffers are an optional optimization,
  not a correctness fix; retain the existing owned-buffer API.
- cros-codecs
  [`a050c6cde530e563197ee9e3e72ab01f48a0c41f`](https://github.com/chromeos/cros-codecs/commit/a050c6cde530e563197ee9e3e72ab01f48a0c41f):
  the only commit absent from the vendor baseline removes
  `log/release_max_level_debug`, which conflicts with a downstream application's
  release log level. Ported in
  [`6d38f3d0`](https://github.com/moq-dev/vaapi/commit/6d38f3d02e2709962789ad73698a27b548fd582e),
  with a feature-unification compilation check in `just ci`. That check failed
  before the fix and passes after it. VA-API's README records every disposition
  and both full checked heads.
- GitHub's published advisory endpoints for both ChromeOS repositories and
  both Discord forks returned empty lists, as did the global Rust advisory
  queries for `cros-libva` and `cros-codecs`. This does not cover undisclosed
  vulnerabilities.

Rechecked 2026-11-02: both ChromeOS default-branch heads above are unchanged,
so no new commits need porting. The four repository advisory endpoints and
both global Rust advisory queries remain empty. The completed audit and its
dispositions still apply.

## Required

- [UniFFI generator forks are current](/quest/m1/vendor-sync/uniffi.md) - the generator forks carry upstream's fixes until rust2 replaces them

## Related

- [Dependency audit](/quest/m2/audit-deps.md) - covers crates cargo-deny can see
