# [XS] moq-vaapi has every ChromeOS fix it needs

## Goal

[moq-dev/vaapi](https://github.com/moq-dev/vaapi) carries every fix that
[chromeos/cros-libva](https://github.com/chromeos/cros-libva) and
[chromeos/cros-codecs](https://github.com/chromeos/cros-codecs) have landed
since the last check, for the code it vendors, each ported with its test or
recorded as not applying, and released to crates.io if anything changed, with
moq's `moq-vaapi` dependency bumped to that release.

## Plan

Recurring child of the [vendor sync](/quest/m3/vendor-sync/README.md), which
says how each run works and recurs. moq-vaapi lives in its own repository and
ships from crates.io for build reasons; its README's `Upstream` section is the
record.

moq-vaapi vendors Discord's forks: cros-libva branch `discord-0.0.13`
([`896a248e`](https://github.com/discord/cros-libva/commit/896a248e003caae7a7120812f7eaa423c93a920d))
and cros-codecs branch `discord-0.0.5`
([`9cdecb19`](https://github.com/discord/cros-codecs/commit/9cdecb192ee3682950d02bd80a7f106163531b76)).
Track the ChromeOS default branches against these diverged baselines, without
undoing Discord's additions.

Last checked (2026-10-31), per [moq-vaapi 0.1.1's Upstream
audit](https://github.com/moq-dev/vaapi/blob/v0.1.1/README.md#upstream):
cros-libva through
[`59384456`](https://github.com/chromeos/cros-libva/commit/59384456ac2ae78c0c3e5515f41ef1efd9b802cf)
and cros-codecs through
[`a050c6cd`](https://github.com/chromeos/cros-codecs/commit/a050c6cde530e563197ee9e3e72ab01f48a0c41f).
That cycle ported the `log/release_max_level_debug` removal
([moq-dev/vaapi#5](https://github.com/moq-dev/vaapi/pull/5)), made the repo's
CI read-only ([moq-dev/vaapi#6](https://github.com/moq-dev/vaapi/pull/6)),
and moq adopted `moq-vaapi 0.1.1`. Ruled out: protected-content headers and
borrowed input buffers have no consumer; the VP9 wrapper already zero-fills
libva 2.23's fields. Advisory checks cover both ChromeOS repositories, both
Discord forks, and the global Rust advisories for `cros-libva` and
`cros-codecs`.

## Required

- [Cut and publish the prepared breaking release](/quest/m3/release/cut.md) - the next cycle starts after the cut
