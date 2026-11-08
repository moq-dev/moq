# [XS] moq-vaapi has every ChromeOS fix it needs

## Goal

`rs/moq-vaapi` carries every fix that
[chromeos/cros-libva](https://github.com/chromeos/cros-libva) and
[chromeos/cros-codecs](https://github.com/chromeos/cros-codecs) have landed
since the last check, for the code it vendors, each ported with its test or
recorded as not applying.

## Plan

Recurring child of the [vendor sync](/quest/m3/vendor-sync/README.md), which
says how each run works and recurs. The crate's README `Upstream` section is
the record: the Discord baselines it forked from, the ChromeOS heads last
checked, and what was ruled out. Track the ChromeOS default branches against
those diverged baselines, without undoing Discord's additions.

The crate moved in-tree from moq-dev/vaapi after its 0.2.1 release; the
previous cycle's audit lives in that README unchanged.

## Required

- [Cut and publish the prepared breaking release](/quest/m3/release/cut.md) - the next cycle starts after the cut
