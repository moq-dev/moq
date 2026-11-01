# [XS] moq-vaapi has every cros-codecs fix it needs

## Goal

[moq-dev/vaapi](https://github.com/moq-dev/vaapi) carries every fix that
[chromeos/cros-libva](https://github.com/chromeos/cros-libva) and
[chromeos/cros-codecs](https://github.com/chromeos/cros-codecs) have landed
since the last check, for the code it vendors, each ported with its test or
recorded as not applying, and released to crates.io if anything changed.

## Plan

Recurring child of the [vendor sync](/quest/m1/vendor-sync/README.md), which
says how each run works and recurs. moq-vaapi lives in its own repository and
ships from crates.io for build reasons, but is vendored code all the same. Port
fixes there and bump moq's `moq-vaapi` version in the same cycle.

moq-vaapi vendors Discord's forks: cros-libva tag `discord-0.0.13`
([`896a248e`](https://github.com/discord/cros-libva/commit/896a248e003caae7a7120812f7eaa423c93a920d),
2026-03-13) and cros-codecs tag `discord-0.0.5`
([`9cdecb19`](https://github.com/discord/cros-codecs/commit/9cdecb192ee3682950d02bd80a7f106163531b76),
2026-06-15). Discord tagged nothing newer by 2026-10-30, so the chromeos
upstreams are the ones to track.

Last checked: nothing beyond those tags. On 2026-10-30 chromeos/cros-libva was
at `59384456`, five commits ahead, of which `59384456` (a VP9 struct field for
libva 2.23+) is the only fix and moq-vaapi is H.264-only, and
chromeos/cros-codecs was at `a050c6cd`, one commit ahead. Record the commits
checked in moq-dev/vaapi's README, and copy them here for the next run.
