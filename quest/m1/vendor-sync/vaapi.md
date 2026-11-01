# [XS] moq-vaapi has every cros-codecs fix it needs

## Goal

[moq-dev/vaapi](https://github.com/moq-dev/vaapi) carries every fix that
[chromeos/cros-libva](https://github.com/chromeos/cros-libva) and
[chromeos/cros-codecs](https://github.com/chromeos/cros-codecs) have landed
since the last check, for the code it vendors, each ported with its test or
recorded as not applying, and released to crates.io if anything changed, with
moq's `moq-vaapi` dependency bumped to that release.

## Plan

Recurring child of the [vendor sync](/quest/m1/vendor-sync/README.md), which
says how each run works and recurs. moq-vaapi lives in its own repository and
ships from crates.io for build reasons. Carry these checked heads and
dispositions into the next cycle when the parent re-creates this child.

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

Remaining this cycle: merge the ported fix,
[moq-dev/vaapi#5](https://github.com/moq-dev/vaapi/pull/5), publish a new
`moq-vaapi` release to crates.io, then bump moq's workspace `moq-vaapi = "0.1.0"`
and lockfile to it and run the affected checks. Public API and wire formats are
unchanged; applications regain control of their release log level.
