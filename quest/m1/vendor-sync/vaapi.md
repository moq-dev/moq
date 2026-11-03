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

Rechecked 2026-11-02: both ChromeOS default-branch heads above are unchanged,
so no new commits need porting. The four repository advisory endpoints and
both global Rust advisory queries remain empty. The completed audit and its
dispositions still apply.

Publication checked 2026-11-02: the crates.io sparse index still ends at
`0.1.0` (2026-09-24), whose `log` dependency still enables
`release_max_level_debug`. VAAPI's newest tag is `v0.1.0`; the source PR is
still a draft at
[`7b013835`](https://github.com/moq-dev/vaapi/commit/7b013835f2fe69e60972c7ffb7ed8602ce8a633e),
which contains the tested fix `6d38f3d0` followed by a documentation-only
commit. Its local Claude review has not completed. Review the PR's current
head before merge and verify the published crate contains the fix. Do not
duplicate the port, adopt a git dependency, or bump MoQ before that release.

CI correction from the maintainer's 2026-11-01 follow-up planning is ready in
[moq-dev/vaapi#6](https://github.com/moq-dev/vaapi/pull/6): remove workflow-wide
`id-token: write`, keep `contents: read`, and explicitly select GitHub Actions
caching with FlakeHub disabled. The pinned Magic Nix Cache action supports
this without OIDC; its
[upstream no-OIDC job](https://github.com/DeterminateSystems/magic-nix-cache-action/actions/runs/25946048507/job/76274417263)
passed cache liveness, upload, and substitution checks. Local VAAPI `just ci`,
actionlint, zizmor, and the Claude review passed. VAAPI's own
[PR check](https://github.com/moq-dev/vaapi/actions/runs/41594404881/job/125778271103)
also passed at `99a8b072aeee9c57099d1e1c6c1bdf8c45792e04`: its log shows
`Contents: read` with no OIDC permission, the native GitHub cache enabled,
and successful Nix store uploads. No protected write job is needed. This
correction has not merged.

Public API and wire formats are unchanged; applications regain control of
their release log level once MoQ adopts the published fix.

## Waiting

- moq-dev/vaapi: review and merge [the existing source port](https://github.com/moq-dev/vaapi/pull/5), and merge [the read-only CI correction](https://github.com/moq-dev/vaapi/pull/6). Require clean local reviews and passing hosted checks on the heads being merged; neither is merged as of 2026-11-02.
- Maintainer: publish a new `moq-vaapi` crate containing the source fix after it merges. Confirm the crates.io release no longer enables `log/release_max_level_debug`, then bump MoQ's workspace requirement and lockfile and run the affected checks. Recommend the next patch release; retaining `0.1.0` leaves the logging conflict unresolved.
