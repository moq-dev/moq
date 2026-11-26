# [M] Investigate rust/relay/relay session announce/moq-lite-06/broadcasts=4096/viewers=64 time, rust/relay/relay ses…

## Goal

Kixel Quest measured what the Plan lists and filed it here on its own. For each finding:

1. Diagnose its cause, starting from the culprit's diff and sessions where it has them.
2. Have a second session, one that did not write the diagnosis, try to refute it, and diagnose again when it does.
3. Fix it, accept it with the reason recorded in this Plan, or improve on it further. A fix carries the measurement that shows it.

Then look past the finding: make the related improvements the diagnosis turns up where they fit here, and add the rest as follow-up quests.

## Plan

Each finding keeps the comment Kixel Quest finds it by, so a later run that sees it again adds its sighting here instead of filing it anew.

### `rust/relay/relay_session_announce/moq-lite-06/broadcasts=4096/viewers=64 time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_announce/moq\u002dlite\u002d06/broadcasts=4096/viewers=64 time","culprit":null} -->

- Series: [`rust/relay/relay_session_announce/moq-lite-06/broadcasts=4096/viewers=64 time`](https://kixel.quest/factory/series/18447), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-26: 17.01 % against a baseline of 5 %, +12.01 % (+240.2%), at [`4fba5df`](https://github.com/moq-dev/moq/commit/4fba5df73f7ef73743a6ebdfdc1550a3dc83ed2a). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 0.0000094.

### `rust/relay/relay_session_churn_held/moq-lite-06/sessions=16/held=1 time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_churn_held/moq\u002dlite\u002d06/sessions=16/held=1 time","culprit":null} -->

- Series: [`rust/relay/relay_session_churn_held/moq-lite-06/sessions=16/held=1 time`](https://kixel.quest/factory/series/18456), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-26: 12.23 % against a baseline of 5 %, +7.234 % (+144.7%), at [`4fba5df`](https://github.com/moq-dev/moq/commit/4fba5df73f7ef73743a6ebdfdc1550a3dc83ed2a). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 0.0025.

### `rust/relay/relay_session_churn_held/moq-lite-06/sessions=16/held=1024 time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_churn_held/moq\u002dlite\u002d06/sessions=16/held=1024 time","culprit":null} -->

- Series: [`rust/relay/relay_session_churn_held/moq-lite-06/sessions=16/held=1024 time`](https://kixel.quest/factory/series/18457), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-26: 11.24 % against a baseline of 5 %, +6.238 % (+124.8%), at [`4fba5df`](https://github.com/moq-dev/moq/commit/4fba5df73f7ef73743a6ebdfdc1550a3dc83ed2a). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 0.0064.

### `rust/relay/relay_session_churn_held/moq-transport-22/sessions=4/held=1024 time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_churn_held/moq\u002dtransport\u002d22/sessions=4/held=1024 time","culprit":null} -->

- Series: [`rust/relay/relay_session_churn_held/moq-transport-22/sessions=4/held=1024 time`](https://kixel.quest/factory/series/18472), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-26: 15.93 % against a baseline of 5 %, +10.93 % (+218.6%), at [`4fba5df`](https://github.com/moq-dev/moq/commit/4fba5df73f7ef73743a6ebdfdc1550a3dc83ed2a). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 0.000039.

### `rust/relay/relay_session_churn_sessions/moq-transport-22/sessions=16/held=16 time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_churn_sessions/moq\u002dtransport\u002d22/sessions=16/held=16 time","culprit":null} -->

- Series: [`rust/relay/relay_session_churn_sessions/moq-transport-22/sessions=16/held=16 time`](https://kixel.quest/factory/series/18478), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-26: 12.37 % against a baseline of 5 %, +7.373 % (+147.5%), at [`4fba5df`](https://github.com/moq-dev/moq/commit/4fba5df73f7ef73743a6ebdfdc1550a3dc83ed2a). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 0.0022.

### `rust/relay/relay_session_dash_relays/moq-lite-06/relays=17/projects=2/tracks=24/total=768 time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_dash_relays/moq\u002dlite\u002d06/relays=17/projects=2/tracks=24/total=768 time","culprit":null} -->

- Series: [`rust/relay/relay_session_dash_relays/moq-lite-06/relays=17/projects=2/tracks=24/total=768 time`](https://kixel.quest/factory/series/18486), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-26: 12.61 % against a baseline of 5 %, +7.609 % (+152.2%), at [`4fba5df`](https://github.com/moq-dev/moq/commit/4fba5df73f7ef73743a6ebdfdc1550a3dc83ed2a). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 0.0017.

### `rust/relay/relay_session_dash_relays/moq-lite-06/relays=2/projects=32/tracks=24/total=768 time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_dash_relays/moq\u002dlite\u002d06/relays=2/projects=32/tracks=24/total=768 time","culprit":null} -->

- Series: [`rust/relay/relay_session_dash_relays/moq-lite-06/relays=2/projects=32/tracks=24/total=768 time`](https://kixel.quest/factory/series/18487), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-26: 11.36 % against a baseline of 5 %, +6.364 % (+127.3%), at [`4fba5df`](https://github.com/moq-dev/moq/commit/4fba5df73f7ef73743a6ebdfdc1550a3dc83ed2a). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 0.0057.

### `rust/relay/relay_session_dash_relays/moq-lite-06/relays=33/projects=1/tracks=24/total=768 time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_dash_relays/moq\u002dlite\u002d06/relays=33/projects=1/tracks=24/total=768 time","culprit":null} -->

- Series: [`rust/relay/relay_session_dash_relays/moq-lite-06/relays=33/projects=1/tracks=24/total=768 time`](https://kixel.quest/factory/series/18488), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-26: 10.82 % against a baseline of 5 %, +5.819 % (+116.4%), at [`4fba5df`](https://github.com/moq-dev/moq/commit/4fba5df73f7ef73743a6ebdfdc1550a3dc83ed2a). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 0.0093.

### `rust/relay/relay_session_dash_relays/moq-transport-22/relays=3/projects=16/tracks=24/total=768 time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_dash_relays/moq\u002dtransport\u002d22/relays=3/projects=16/tracks=24/total=768 time","culprit":null} -->

- Series: [`rust/relay/relay_session_dash_relays/moq-transport-22/relays=3/projects=16/tracks=24/total=768 time`](https://kixel.quest/factory/series/18497), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-26: 16.43 % against a baseline of 5 %, +11.43 % (+228.5%), at [`4fba5df`](https://github.com/moq-dev/moq/commit/4fba5df73f7ef73743a6ebdfdc1550a3dc83ed2a). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 0.000021.

### `rust/relay/relay_session_dash_tracks/moq-transport-22/relays=5/projects=4/tracks=96/total=1536 time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_dash_tracks/moq\u002dtransport\u002d22/relays=5/projects=4/tracks=96/total=1536 time","culprit":null} -->

- Series: [`rust/relay/relay_session_dash_tracks/moq-transport-22/relays=5/projects=4/tracks=96/total=1536 time`](https://kixel.quest/factory/series/18505), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-26: 12.69 % against a baseline of 5 %, +7.69 % (+153.8%), at [`4fba5df`](https://github.com/moq-dev/moq/commit/4fba5df73f7ef73743a6ebdfdc1550a3dc83ed2a). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 0.0016.

### `rust/relay/relay_session_delivery_broadcasts/moq-transport-22/relays=1/publishers=1/broadcasts=256/viewers=16/watch=1/frame=64 time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_delivery_broadcasts/moq\u002dtransport\u002d22/relays=1/publishers=1/broadcasts=256/viewers=16/watch=1/frame=64 time","culprit":null} -->

- Series: [`rust/relay/relay_session_delivery_broadcasts/moq-transport-22/relays=1/publishers=1/broadcasts=256/viewers=16/watch=1/frame=64 time`](https://kixel.quest/factory/series/18513), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-26: 11.35 % against a baseline of 5 %, +6.346 % (+126.9%), at [`4fba5df`](https://github.com/moq-dev/moq/commit/4fba5df73f7ef73743a6ebdfdc1550a3dc83ed2a). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 0.0058.

### `rust/relay/relay_session_delivery_broadcasts/moq-transport-22/relays=1/publishers=1/broadcasts=4096/viewers=16/watch=1/frame=64 time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_delivery_broadcasts/moq\u002dtransport\u002d22/relays=1/publishers=1/broadcasts=4096/viewers=16/watch=1/frame=64 time","culprit":null} -->

- Series: [`rust/relay/relay_session_delivery_broadcasts/moq-transport-22/relays=1/publishers=1/broadcasts=4096/viewers=16/watch=1/frame=64 time`](https://kixel.quest/factory/series/18514), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-26: 11.8 % against a baseline of 5 %, +6.803 % (+136.1%), at [`4fba5df`](https://github.com/moq-dev/moq/commit/4fba5df73f7ef73743a6ebdfdc1550a3dc83ed2a). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 0.0038.

### `rust/relay/relay_session_delivery_burst/moq-transport-22/relays=1/publishers=1/broadcasts=16/viewers=1/watch=16/frame=64/burst=16 time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_delivery_burst/moq\u002dtransport\u002d22/relays=1/publishers=1/broadcasts=16/viewers=1/watch=16/frame=64/burst=16 time","culprit":null} -->

- Series: [`rust/relay/relay_session_delivery_burst/moq-transport-22/relays=1/publishers=1/broadcasts=16/viewers=1/watch=16/frame=64/burst=16 time`](https://kixel.quest/factory/series/18534), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-26: 14.57 % against a baseline of 5 %, +9.575 % (+191.5%), at [`4fba5df`](https://github.com/moq-dev/moq/commit/4fba5df73f7ef73743a6ebdfdc1550a3dc83ed2a). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 0.00020.

### `rust/relay/relay_session_delivery_compression/moq-lite-06/relays=1/publishers=16/broadcasts=1/viewers=16/watch=1/frame=1024 time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_delivery_compression/moq\u002dlite\u002d06/relays=1/publishers=16/broadcasts=1/viewers=16/watch=1/frame=1024 time","culprit":null} -->

- Series: [`rust/relay/relay_session_delivery_compression/moq-lite-06/relays=1/publishers=16/broadcasts=1/viewers=16/watch=1/frame=1024 time`](https://kixel.quest/factory/series/18536), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-26: 17.21 % against a baseline of 5 %, +12.21 % (+244.1%), at [`4fba5df`](https://github.com/moq-dev/moq/commit/4fba5df73f7ef73743a6ebdfdc1550a3dc83ed2a). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 0.0000072.

### `rust/relay/relay_session_delivery_compression/moq-transport-22/relays=1/publishers=16/broadcasts=1/viewers=16/watch=1/frame=1024 time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_delivery_compression/moq\u002dtransport\u002d22/relays=1/publishers=16/broadcasts=1/viewers=16/watch=1/frame=1024 time","culprit":null} -->

- Series: [`rust/relay/relay_session_delivery_compression/moq-transport-22/relays=1/publishers=16/broadcasts=1/viewers=16/watch=1/frame=1024 time`](https://kixel.quest/factory/series/18544), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-26: 11.89 % against a baseline of 5 %, +6.885 % (+137.7%), at [`4fba5df`](https://github.com/moq-dev/moq/commit/4fba5df73f7ef73743a6ebdfdc1550a3dc83ed2a). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 0.0035.

### `rust/relay/relay_session_delivery_credit/moq-lite-06/watch=64/burst=1/credit=1 time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_delivery_credit/moq\u002dlite\u002d06/watch=64/burst=1/credit=1 time","culprit":null} -->

- Series: [`rust/relay/relay_session_delivery_credit/moq-lite-06/watch=64/burst=1/credit=1 time`](https://kixel.quest/factory/series/18555), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-26: 26.18 % against a baseline of 5 %, +21.18 % (+423.6%), at [`4fba5df`](https://github.com/moq-dev/moq/commit/4fba5df73f7ef73743a6ebdfdc1550a3dc83ed2a). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 1.2e-12.

### `rust/relay/relay_session_delivery_credit/moq-lite-07-wip/watch=64/burst=1/credit=1 time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_delivery_credit/moq\u002dlite\u002d07\u002dwip/watch=64/burst=1/credit=1 time","culprit":null} -->

- Series: [`rust/relay/relay_session_delivery_credit/moq-lite-07-wip/watch=64/burst=1/credit=1 time`](https://kixel.quest/factory/series/18564), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-26: 18.42 % against a baseline of 5 %, +13.42 % (+268.4%), at [`4fba5df`](https://github.com/moq-dev/moq/commit/4fba5df73f7ef73743a6ebdfdc1550a3dc83ed2a). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 0.0000013.

### `rust/relay/relay_session_delivery_credit/moq-transport-22/watch=16/burst=16/credit=1 time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_delivery_credit/moq\u002dtransport\u002d22/watch=16/burst=16/credit=1 time","culprit":null} -->

- Series: [`rust/relay/relay_session_delivery_credit/moq-transport-22/watch=16/burst=16/credit=1 time`](https://kixel.quest/factory/series/18566), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-26: 11.29 % against a baseline of 5 %, +6.291 % (+125.8%), at [`4fba5df`](https://github.com/moq-dev/moq/commit/4fba5df73f7ef73743a6ebdfdc1550a3dc83ed2a). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 0.0061.

### `rust/relay/relay_session_delivery_datagram/moq-lite-07-wip/publishers=16/viewers=16/payload=1024/cadence=1 time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_delivery_datagram/moq\u002dlite\u002d07\u002dwip/publishers=16/viewers=16/payload=1024/cadence=1 time","culprit":null} -->

- Series: [`rust/relay/relay_session_delivery_datagram/moq-lite-07-wip/publishers=16/viewers=16/payload=1024/cadence=1 time`](https://kixel.quest/factory/series/18584), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-26: 13.36 % against a baseline of 5 %, +8.357 % (+167.1%), at [`4fba5df`](https://github.com/moq-dev/moq/commit/4fba5df73f7ef73743a6ebdfdc1550a3dc83ed2a). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 0.00080.

### `rust/relay/relay_session_delivery_datagram/moq-transport-22/publishers=16/viewers=64/payload=64/cadence=1 time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_delivery_datagram/moq\u002dtransport\u002d22/publishers=16/viewers=64/payload=64/cadence=1 time","culprit":null} -->

- Series: [`rust/relay/relay_session_delivery_datagram/moq-transport-22/publishers=16/viewers=64/payload=64/cadence=1 time`](https://kixel.quest/factory/series/18598), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-26: 11.09 % against a baseline of 5 %, +6.09 % (+121.8%), at [`4fba5df`](https://github.com/moq-dev/moq/commit/4fba5df73f7ef73743a6ebdfdc1550a3dc83ed2a). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 0.0073.

### `rust/relay/relay_session_delivery_fetch_frames/moq-lite-07-wip/relays=1/publishers=4/broadcasts=1/viewers=16/watch=1/frame=64/fetch time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_delivery_fetch_frames/moq\u002dlite\u002d07\u002dwip/relays=1/publishers=4/broadcasts=1/viewers=16/watch=1/frame=64/fetch time","culprit":null} -->

- Series: [`rust/relay/relay_session_delivery_fetch_frames/moq-lite-07-wip/relays=1/publishers=4/broadcasts=1/viewers=16/watch=1/frame=64/fetch time`](https://kixel.quest/factory/series/18614), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-26: 11.92 % against a baseline of 5 %, +6.917 % (+138.3%), at [`4fba5df`](https://github.com/moq-dev/moq/commit/4fba5df73f7ef73743a6ebdfdc1550a3dc83ed2a). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 0.0034.

### `rust/relay/relay_session_delivery_fetch_range/moq-lite-07-wip/publishers=16/viewers=16/burst=2/fetch time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_delivery_fetch_range/moq\u002dlite\u002d07\u002dwip/publishers=16/viewers=16/burst=2/fetch time","culprit":null} -->

- Series: [`rust/relay/relay_session_delivery_fetch_range/moq-lite-07-wip/publishers=16/viewers=16/burst=2/fetch time`](https://kixel.quest/factory/series/18635), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-26: 11.93 % against a baseline of 5 %, +6.933 % (+138.7%), at [`4fba5df`](https://github.com/moq-dev/moq/commit/4fba5df73f7ef73743a6ebdfdc1550a3dc83ed2a). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 0.0034.

### `rust/relay/relay_session_delivery_fetch_range/moq-transport-22/publishers=16/viewers=16/burst=2/fetch time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_delivery_fetch_range/moq\u002dtransport\u002d22/publishers=16/viewers=16/burst=2/fetch time","culprit":null} -->

- Series: [`rust/relay/relay_session_delivery_fetch_range/moq-transport-22/publishers=16/viewers=16/burst=2/fetch time`](https://kixel.quest/factory/series/18643), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-26: 11.73 % against a baseline of 5 %, +6.728 % (+134.6%), at [`4fba5df`](https://github.com/moq-dev/moq/commit/4fba5df73f7ef73743a6ebdfdc1550a3dc83ed2a). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 0.0041.

### `rust/relay/relay_session_delivery_frame/moq-lite-07-wip/relays=1/publishers=16/broadcasts=1/viewers=16/watch=1/frame=1024 time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_delivery_frame/moq\u002dlite\u002d07\u002dwip/relays=1/publishers=16/broadcasts=1/viewers=16/watch=1/frame=1024 time","culprit":null} -->

- Series: [`rust/relay/relay_session_delivery_frame/moq-lite-07-wip/relays=1/publishers=16/broadcasts=1/viewers=16/watch=1/frame=1024 time`](https://kixel.quest/factory/series/18654), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-26: 16.36 % against a baseline of 5 %, +11.36 % (+227.3%), at [`4fba5df`](https://github.com/moq-dev/moq/commit/4fba5df73f7ef73743a6ebdfdc1550a3dc83ed2a). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 0.000022.

### `rust/relay/relay_session_delivery_frame/moq-lite-07-wip/relays=1/publishers=16/broadcasts=1/viewers=16/watch=1/frame=16384 time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_delivery_frame/moq\u002dlite\u002d07\u002dwip/relays=1/publishers=16/broadcasts=1/viewers=16/watch=1/frame=16384 time","culprit":null} -->

- Series: [`rust/relay/relay_session_delivery_frame/moq-lite-07-wip/relays=1/publishers=16/broadcasts=1/viewers=16/watch=1/frame=16384 time`](https://kixel.quest/factory/series/18655), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-26: 17.4 % against a baseline of 5 %, +12.4 % (+248%), at [`4fba5df`](https://github.com/moq-dev/moq/commit/4fba5df73f7ef73743a6ebdfdc1550a3dc83ed2a). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 0.0000055.

### `rust/relay/relay_session_delivery_frame/moq-lite-07-wip/relays=1/publishers=16/broadcasts=1/viewers=16/watch=1/frame=64 time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_delivery_frame/moq\u002dlite\u002d07\u002dwip/relays=1/publishers=16/broadcasts=1/viewers=16/watch=1/frame=64 time","culprit":null} -->

- Series: [`rust/relay/relay_session_delivery_frame/moq-lite-07-wip/relays=1/publishers=16/broadcasts=1/viewers=16/watch=1/frame=64 time`](https://kixel.quest/factory/series/18653), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-26: 14.33 % against a baseline of 5 %, +9.328 % (+186.6%), at [`4fba5df`](https://github.com/moq-dev/moq/commit/4fba5df73f7ef73743a6ebdfdc1550a3dc83ed2a). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 0.00027.

### `rust/relay/relay_session_delivery_frame/moq-transport-22/relays=1/publishers=16/broadcasts=1/viewers=16/watch=1/frame=16384 time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_delivery_frame/moq\u002dtransport\u002d22/relays=1/publishers=16/broadcasts=1/viewers=16/watch=1/frame=16384 time","culprit":null} -->

- Series: [`rust/relay/relay_session_delivery_frame/moq-transport-22/relays=1/publishers=16/broadcasts=1/viewers=16/watch=1/frame=16384 time`](https://kixel.quest/factory/series/18658), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-26: 13.68 % against a baseline of 5 %, +8.682 % (+173.6%), at [`4fba5df`](https://github.com/moq-dev/moq/commit/4fba5df73f7ef73743a6ebdfdc1550a3dc83ed2a). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 0.00056.

### `rust/relay/relay_session_delivery_frame/moq-transport-22/relays=1/publishers=16/broadcasts=1/viewers=16/watch=1/frame=64 time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_delivery_frame/moq\u002dtransport\u002d22/relays=1/publishers=16/broadcasts=1/viewers=16/watch=1/frame=64 time","culprit":null} -->

- Series: [`rust/relay/relay_session_delivery_frame/moq-transport-22/relays=1/publishers=16/broadcasts=1/viewers=16/watch=1/frame=64 time`](https://kixel.quest/factory/series/18656), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-26: 12.39 % against a baseline of 5 %, +7.395 % (+147.9%), at [`4fba5df`](https://github.com/moq-dev/moq/commit/4fba5df73f7ef73743a6ebdfdc1550a3dc83ed2a). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 0.0021.

### `rust/relay/relay_session_delivery_group_cost/moq-lite-06/relays=1/publishers=1/broadcasts=1/viewers=16/watch=1/frame=64/fetch time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_delivery_group_cost/moq\u002dlite\u002d06/relays=1/publishers=1/broadcasts=1/viewers=16/watch=1/frame=64/fetch time","culprit":null} -->

- Series: [`rust/relay/relay_session_delivery_group_cost/moq-lite-06/relays=1/publishers=1/broadcasts=1/viewers=16/watch=1/frame=64/fetch time`](https://kixel.quest/factory/series/18674), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-26: 10.96 % against a baseline of 5 %, +5.96 % (+119.2%), at [`4fba5df`](https://github.com/moq-dev/moq/commit/4fba5df73f7ef73743a6ebdfdc1550a3dc83ed2a). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 0.0082.

### `rust/relay/relay_session_delivery_group_cost/moq-lite-06/relays=1/publishers=1/broadcasts=1/viewers=64/watch=1/frame=64/fetch time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_delivery_group_cost/moq\u002dlite\u002d06/relays=1/publishers=1/broadcasts=1/viewers=64/watch=1/frame=64/fetch time","culprit":null} -->

- Series: [`rust/relay/relay_session_delivery_group_cost/moq-lite-06/relays=1/publishers=1/broadcasts=1/viewers=64/watch=1/frame=64/fetch time`](https://kixel.quest/factory/series/18682), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-26: 13.42 % against a baseline of 5 %, +8.423 % (+168.5%), at [`4fba5df`](https://github.com/moq-dev/moq/commit/4fba5df73f7ef73743a6ebdfdc1550a3dc83ed2a). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 0.00074.

### `rust/relay/relay_session_delivery_group_cost/moq-lite-06/relays=1/publishers=1/broadcasts=1/viewers=64/watch=1/frame=64/window=64 time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_delivery_group_cost/moq\u002dlite\u002d06/relays=1/publishers=1/broadcasts=1/viewers=64/watch=1/frame=64/window=64 time","culprit":null} -->

- Series: [`rust/relay/relay_session_delivery_group_cost/moq-lite-06/relays=1/publishers=1/broadcasts=1/viewers=64/watch=1/frame=64/window=64 time`](https://kixel.quest/factory/series/18679), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-26: 16.88 % against a baseline of 5 %, +11.88 % (+237.6%), at [`4fba5df`](https://github.com/moq-dev/moq/commit/4fba5df73f7ef73743a6ebdfdc1550a3dc83ed2a). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 0.000011.

### `rust/relay/relay_session_delivery_group_cost/moq-lite-06/relays=1/publishers=16/broadcasts=1/viewers=64/watch=1/frame=64/fetch time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_delivery_group_cost/moq\u002dlite\u002d06/relays=1/publishers=16/broadcasts=1/viewers=64/watch=1/frame=64/fetch time","culprit":null} -->

- Series: [`rust/relay/relay_session_delivery_group_cost/moq-lite-06/relays=1/publishers=16/broadcasts=1/viewers=64/watch=1/frame=64/fetch time`](https://kixel.quest/factory/series/18670), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-26: 10.78 % against a baseline of 5 %, +5.781 % (+115.6%), at [`4fba5df`](https://github.com/moq-dev/moq/commit/4fba5df73f7ef73743a6ebdfdc1550a3dc83ed2a). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 0.0096.

### `rust/relay/relay_session_delivery_group_cost/moq-lite-07-wip/relays=1/publishers=1/broadcasts=1/viewers=64/watch=1/frame=64 time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_delivery_group_cost/moq\u002dlite\u002d07\u002dwip/relays=1/publishers=1/broadcasts=1/viewers=64/watch=1/frame=64 time","culprit":null} -->

- Series: [`rust/relay/relay_session_delivery_group_cost/moq-lite-07-wip/relays=1/publishers=1/broadcasts=1/viewers=64/watch=1/frame=64 time`](https://kixel.quest/factory/series/18705), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-26: 17.05 % against a baseline of 5 %, +12.05 % (+241.1%), at [`4fba5df`](https://github.com/moq-dev/moq/commit/4fba5df73f7ef73743a6ebdfdc1550a3dc83ed2a). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 0.0000089.

### `rust/relay/relay_session_delivery_group_cost/moq-lite-07-wip/relays=1/publishers=1/broadcasts=1/viewers=64/watch=1/frame=64/fetch time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_delivery_group_cost/moq\u002dlite\u002d07\u002dwip/relays=1/publishers=1/broadcasts=1/viewers=64/watch=1/frame=64/fetch time","culprit":null} -->

- Series: [`rust/relay/relay_session_delivery_group_cost/moq-lite-07-wip/relays=1/publishers=1/broadcasts=1/viewers=64/watch=1/frame=64/fetch time`](https://kixel.quest/factory/series/18706), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-26: 13.18 % against a baseline of 5 %, +8.176 % (+163.5%), at [`4fba5df`](https://github.com/moq-dev/moq/commit/4fba5df73f7ef73743a6ebdfdc1550a3dc83ed2a). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 0.00096.

### `rust/relay/relay_session_delivery_group_cost/moq-lite-07-wip/relays=1/publishers=1/broadcasts=1/viewers=64/watch=1/frame=64/window=64 time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_delivery_group_cost/moq\u002dlite\u002d07\u002dwip/relays=1/publishers=1/broadcasts=1/viewers=64/watch=1/frame=64/window=64 time","culprit":null} -->

- Series: [`rust/relay/relay_session_delivery_group_cost/moq-lite-07-wip/relays=1/publishers=1/broadcasts=1/viewers=64/watch=1/frame=64/window=64 time`](https://kixel.quest/factory/series/18703), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-26: 23.67 % against a baseline of 5 %, +18.67 % (+373.4%), at [`4fba5df`](https://github.com/moq-dev/moq/commit/4fba5df73f7ef73743a6ebdfdc1550a3dc83ed2a). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 1.8e-10.

### `rust/relay/relay_session_delivery_group_cost/moq-lite-07-wip/relays=1/publishers=1/broadcasts=1/viewers=64/watch=1/frame=64/window=64/fetch time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_delivery_group_cost/moq\u002dlite\u002d07\u002dwip/relays=1/publishers=1/broadcasts=1/viewers=64/watch=1/frame=64/window=64/fetch time","culprit":null} -->

- Series: [`rust/relay/relay_session_delivery_group_cost/moq-lite-07-wip/relays=1/publishers=1/broadcasts=1/viewers=64/watch=1/frame=64/window=64/fetch time`](https://kixel.quest/factory/series/18704), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-26: 13.62 % against a baseline of 5 %, +8.617 % (+172.3%), at [`4fba5df`](https://github.com/moq-dev/moq/commit/4fba5df73f7ef73743a6ebdfdc1550a3dc83ed2a). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 0.00060.

### `rust/relay/relay_session_delivery_group_cost/moq-lite-07-wip/relays=1/publishers=16/broadcasts=1/viewers=64/watch=1/frame=64 time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_delivery_group_cost/moq\u002dlite\u002d07\u002dwip/relays=1/publishers=16/broadcasts=1/viewers=64/watch=1/frame=64 time","culprit":null} -->

- Series: [`rust/relay/relay_session_delivery_group_cost/moq-lite-07-wip/relays=1/publishers=16/broadcasts=1/viewers=64/watch=1/frame=64 time`](https://kixel.quest/factory/series/18693), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-26: 22.81 % against a baseline of 5 %, +17.81 % (+356.3%), at [`4fba5df`](https://github.com/moq-dev/moq/commit/4fba5df73f7ef73743a6ebdfdc1550a3dc83ed2a). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 9.1e-10.

### `rust/relay/relay_session_delivery_group_cost/moq-lite-07-wip/relays=1/publishers=16/broadcasts=1/viewers=64/watch=1/frame=64/window=64 time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_delivery_group_cost/moq\u002dlite\u002d07\u002dwip/relays=1/publishers=16/broadcasts=1/viewers=64/watch=1/frame=64/window=64 time","culprit":null} -->

- Series: [`rust/relay/relay_session_delivery_group_cost/moq-lite-07-wip/relays=1/publishers=16/broadcasts=1/viewers=64/watch=1/frame=64/window=64 time`](https://kixel.quest/factory/series/18691), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-26: 30.71 % against a baseline of 5 %, +25.71 % (+514.2%), at [`4fba5df`](https://github.com/moq-dev/moq/commit/4fba5df73f7ef73743a6ebdfdc1550a3dc83ed2a). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 4.1e-17.

### `rust/relay/relay_session_delivery_group_cost/moq-lite-07-wip/relays=1/publishers=16/broadcasts=1/viewers=64/watch=1/frame=64/window=64/fetch time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_delivery_group_cost/moq\u002dlite\u002d07\u002dwip/relays=1/publishers=16/broadcasts=1/viewers=64/watch=1/frame=64/window=64/fetch time","culprit":null} -->

- Series: [`rust/relay/relay_session_delivery_group_cost/moq-lite-07-wip/relays=1/publishers=16/broadcasts=1/viewers=64/watch=1/frame=64/window=64/fetch time`](https://kixel.quest/factory/series/18692), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-26: 13.81 % against a baseline of 5 %, +8.807 % (+176.1%), at [`4fba5df`](https://github.com/moq-dev/moq/commit/4fba5df73f7ef73743a6ebdfdc1550a3dc83ed2a). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 0.00049.

### `rust/relay/relay_session_delivery_group_cost/moq-transport-22/relays=1/publishers=1/broadcasts=1/viewers=16/watch=1/frame=64 time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_delivery_group_cost/moq\u002dtransport\u002d22/relays=1/publishers=1/broadcasts=1/viewers=16/watch=1/frame=64 time","culprit":null} -->

- Series: [`rust/relay/relay_session_delivery_group_cost/moq-transport-22/relays=1/publishers=1/broadcasts=1/viewers=16/watch=1/frame=64 time`](https://kixel.quest/factory/series/18721), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-26: 10.9 % against a baseline of 5 %, +5.899 % (+118%), at [`4fba5df`](https://github.com/moq-dev/moq/commit/4fba5df73f7ef73743a6ebdfdc1550a3dc83ed2a). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 0.0086.

### `rust/relay/relay_session_delivery_group_cost/moq-transport-22/relays=1/publishers=1/broadcasts=1/viewers=64/watch=1/frame=64 time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_delivery_group_cost/moq\u002dtransport\u002d22/relays=1/publishers=1/broadcasts=1/viewers=64/watch=1/frame=64 time","culprit":null} -->

- Series: [`rust/relay/relay_session_delivery_group_cost/moq-transport-22/relays=1/publishers=1/broadcasts=1/viewers=64/watch=1/frame=64 time`](https://kixel.quest/factory/series/18729), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-26: 23.78 % against a baseline of 5 %, +18.78 % (+375.5%), at [`4fba5df`](https://github.com/moq-dev/moq/commit/4fba5df73f7ef73743a6ebdfdc1550a3dc83ed2a). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 1.5e-10.

### `rust/relay/relay_session_delivery_group_cost/moq-transport-22/relays=1/publishers=1/broadcasts=1/viewers=64/watch=1/frame=64/window=64 time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_delivery_group_cost/moq\u002dtransport\u002d22/relays=1/publishers=1/broadcasts=1/viewers=64/watch=1/frame=64/window=64 time","culprit":null} -->

- Series: [`rust/relay/relay_session_delivery_group_cost/moq-transport-22/relays=1/publishers=1/broadcasts=1/viewers=64/watch=1/frame=64/window=64 time`](https://kixel.quest/factory/series/18727), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-26: 21.12 % against a baseline of 5 %, +16.12 % (+322.3%), at [`4fba5df`](https://github.com/moq-dev/moq/commit/4fba5df73f7ef73743a6ebdfdc1550a3dc83ed2a). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 1.8e-8.

### `rust/relay/relay_session_delivery_group_cost/moq-transport-22/relays=1/publishers=16/broadcasts=1/viewers=16/watch=1/frame=64/fetch time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_delivery_group_cost/moq\u002dtransport\u002d22/relays=1/publishers=16/broadcasts=1/viewers=16/watch=1/frame=64/fetch time","culprit":null} -->

- Series: [`rust/relay/relay_session_delivery_group_cost/moq-transport-22/relays=1/publishers=16/broadcasts=1/viewers=16/watch=1/frame=64/fetch time`](https://kixel.quest/factory/series/18710), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-26: 15.23 % against a baseline of 5 %, +10.23 % (+204.5%), at [`4fba5df`](https://github.com/moq-dev/moq/commit/4fba5df73f7ef73743a6ebdfdc1550a3dc83ed2a). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 0.000094.

### `rust/relay/relay_session_delivery_group_cost/moq-transport-22/relays=1/publishers=16/broadcasts=1/viewers=16/watch=1/frame=64/window=64/fetch time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_delivery_group_cost/moq\u002dtransport\u002d22/relays=1/publishers=16/broadcasts=1/viewers=16/watch=1/frame=64/window=64/fetch time","culprit":null} -->

- Series: [`rust/relay/relay_session_delivery_group_cost/moq-transport-22/relays=1/publishers=16/broadcasts=1/viewers=16/watch=1/frame=64/window=64/fetch time`](https://kixel.quest/factory/series/18708), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-26: 11.07 % against a baseline of 5 %, +6.066 % (+121.3%), at [`4fba5df`](https://github.com/moq-dev/moq/commit/4fba5df73f7ef73743a6ebdfdc1550a3dc83ed2a). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 0.0075.

### `rust/relay/relay_session_delivery_group_cost/moq-transport-22/relays=1/publishers=16/broadcasts=1/viewers=64/watch=1/frame=64/fetch time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_delivery_group_cost/moq\u002dtransport\u002d22/relays=1/publishers=16/broadcasts=1/viewers=64/watch=1/frame=64/fetch time","culprit":null} -->

- Series: [`rust/relay/relay_session_delivery_group_cost/moq-transport-22/relays=1/publishers=16/broadcasts=1/viewers=64/watch=1/frame=64/fetch time`](https://kixel.quest/factory/series/18718), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-26: 14.49 % against a baseline of 5 %, +9.486 % (+189.7%), at [`4fba5df`](https://github.com/moq-dev/moq/commit/4fba5df73f7ef73743a6ebdfdc1550a3dc83ed2a). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 0.00023.

### `rust/relay/relay_session_delivery_group_cost/moq-transport-22/relays=1/publishers=16/broadcasts=1/viewers=64/watch=1/frame=64/window=64 time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_delivery_group_cost/moq\u002dtransport\u002d22/relays=1/publishers=16/broadcasts=1/viewers=64/watch=1/frame=64/window=64 time","culprit":null} -->

- Series: [`rust/relay/relay_session_delivery_group_cost/moq-transport-22/relays=1/publishers=16/broadcasts=1/viewers=64/watch=1/frame=64/window=64 time`](https://kixel.quest/factory/series/18715), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-26: 28.93 % against a baseline of 5 %, +23.93 % (+478.5%), at [`4fba5df`](https://github.com/moq-dev/moq/commit/4fba5df73f7ef73743a6ebdfdc1550a3dc83ed2a). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 2.9e-15.

### `rust/relay/relay_session_delivery_paced/moq-lite-06/relays=1/publishers=16/broadcasts=1/viewers=256/watch=1/frame=64/paced time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_delivery_paced/moq\u002dlite\u002d06/relays=1/publishers=16/broadcasts=1/viewers=256/watch=1/frame=64/paced time","culprit":null} -->

- Series: [`rust/relay/relay_session_delivery_paced/moq-lite-06/relays=1/publishers=16/broadcasts=1/viewers=256/watch=1/frame=64/paced time`](https://kixel.quest/factory/series/18733), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-26: 20.87 % against a baseline of 5 %, +15.87 % (+317.3%), at [`4fba5df`](https://github.com/moq-dev/moq/commit/4fba5df73f7ef73743a6ebdfdc1550a3dc83ed2a). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 2.7e-8.

### `rust/relay/relay_session_delivery_paced/moq-transport-22/relays=1/publishers=16/broadcasts=1/viewers=16/watch=1/frame=64/paced time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_delivery_paced/moq\u002dtransport\u002d22/relays=1/publishers=16/broadcasts=1/viewers=16/watch=1/frame=64/paced time","culprit":null} -->

- Series: [`rust/relay/relay_session_delivery_paced/moq-transport-22/relays=1/publishers=16/broadcasts=1/viewers=16/watch=1/frame=64/paced time`](https://kixel.quest/factory/series/18737), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-26: 11.31 % against a baseline of 5 %, +6.308 % (+126.2%), at [`4fba5df`](https://github.com/moq-dev/moq/commit/4fba5df73f7ef73743a6ebdfdc1550a3dc83ed2a). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 0.0060.

### `rust/relay/relay_session_delivery_prefixes/moq-transport-22/broadcasts=1025/follow=64 time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_delivery_prefixes/moq\u002dtransport\u002d22/broadcasts=1025/follow=64 time","culprit":null} -->

- Series: [`rust/relay/relay_session_delivery_prefixes/moq-transport-22/broadcasts=1025/follow=64 time`](https://kixel.quest/factory/series/18748), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-26: 11.44 % against a baseline of 5 %, +6.44 % (+128.8%), at [`4fba5df`](https://github.com/moq-dev/moq/commit/4fba5df73f7ef73743a6ebdfdc1550a3dc83ed2a). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 0.0053.

### `rust/relay/relay_session_delivery_publishers/moq-lite-06/relays=1/publishers=16/broadcasts=1/viewers=16/watch=1/frame=64 time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_delivery_publishers/moq\u002dlite\u002d06/relays=1/publishers=16/broadcasts=1/viewers=16/watch=1/frame=64 time","culprit":null} -->

- Series: [`rust/relay/relay_session_delivery_publishers/moq-lite-06/relays=1/publishers=16/broadcasts=1/viewers=16/watch=1/frame=64 time`](https://kixel.quest/factory/series/18749), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-26: 11.55 % against a baseline of 5 %, +6.549 % (+131%), at [`4fba5df`](https://github.com/moq-dev/moq/commit/4fba5df73f7ef73743a6ebdfdc1550a3dc83ed2a). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 0.0048.

### `rust/relay/relay_session_delivery_publishers/moq-lite-06/relays=1/publishers=256/broadcasts=1/viewers=16/watch=1/frame=64 time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_delivery_publishers/moq\u002dlite\u002d06/relays=1/publishers=256/broadcasts=1/viewers=16/watch=1/frame=64 time","culprit":null} -->

- Series: [`rust/relay/relay_session_delivery_publishers/moq-lite-06/relays=1/publishers=256/broadcasts=1/viewers=16/watch=1/frame=64 time`](https://kixel.quest/factory/series/18751), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-26: 14.44 % against a baseline of 5 %, +9.439 % (+188.8%), at [`4fba5df`](https://github.com/moq-dev/moq/commit/4fba5df73f7ef73743a6ebdfdc1550a3dc83ed2a). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 0.00024.

### `rust/relay/relay_session_delivery_publishers/moq-lite-07-wip/relays=1/publishers=16/broadcasts=1/viewers=16/watch=1/frame=64 time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_delivery_publishers/moq\u002dlite\u002d07\u002dwip/relays=1/publishers=16/broadcasts=1/viewers=16/watch=1/frame=64 time","culprit":null} -->

- Series: [`rust/relay/relay_session_delivery_publishers/moq-lite-07-wip/relays=1/publishers=16/broadcasts=1/viewers=16/watch=1/frame=64 time`](https://kixel.quest/factory/series/18752), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-26: 12.15 % against a baseline of 5 %, +7.148 % (+143%), at [`4fba5df`](https://github.com/moq-dev/moq/commit/4fba5df73f7ef73743a6ebdfdc1550a3dc83ed2a). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 0.0027.

### `rust/relay/relay_session_delivery_publishers/moq-lite-07-wip/relays=1/publishers=256/broadcasts=1/viewers=16/watch=1/frame=64 time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_delivery_publishers/moq\u002dlite\u002d07\u002dwip/relays=1/publishers=256/broadcasts=1/viewers=16/watch=1/frame=64 time","culprit":null} -->

- Series: [`rust/relay/relay_session_delivery_publishers/moq-lite-07-wip/relays=1/publishers=256/broadcasts=1/viewers=16/watch=1/frame=64 time`](https://kixel.quest/factory/series/18754), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-26: 12.72 % against a baseline of 5 %, +7.721 % (+154.4%), at [`4fba5df`](https://github.com/moq-dev/moq/commit/4fba5df73f7ef73743a6ebdfdc1550a3dc83ed2a). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 0.0015.

### `rust/relay/relay_session_delivery_publishers/moq-transport-22/relays=1/publishers=16/broadcasts=1/viewers=16/watch=1/frame=64 time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_delivery_publishers/moq\u002dtransport\u002d22/relays=1/publishers=16/broadcasts=1/viewers=16/watch=1/frame=64 time","culprit":null} -->

- Series: [`rust/relay/relay_session_delivery_publishers/moq-transport-22/relays=1/publishers=16/broadcasts=1/viewers=16/watch=1/frame=64 time`](https://kixel.quest/factory/series/18755), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-26: 12.79 % against a baseline of 5 %, +7.792 % (+155.8%), at [`4fba5df`](https://github.com/moq-dev/moq/commit/4fba5df73f7ef73743a6ebdfdc1550a3dc83ed2a). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 0.0014.

### `rust/relay/relay_session_delivery_publishers/moq-transport-22/relays=1/publishers=256/broadcasts=1/viewers=16/watch=1/frame=64 time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_delivery_publishers/moq\u002dtransport\u002d22/relays=1/publishers=256/broadcasts=1/viewers=16/watch=1/frame=64 time","culprit":null} -->

- Series: [`rust/relay/relay_session_delivery_publishers/moq-transport-22/relays=1/publishers=256/broadcasts=1/viewers=16/watch=1/frame=64 time`](https://kixel.quest/factory/series/18757), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-26: 11.69 % against a baseline of 5 %, +6.686 % (+133.7%), at [`4fba5df`](https://github.com/moq-dev/moq/commit/4fba5df73f7ef73743a6ebdfdc1550a3dc83ed2a). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 0.0042.

### `rust/relay/relay_session_delivery_relays/moq-lite-06/relays=2/publishers=16/broadcasts=1/viewers=64/watch=1/frame=64 time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_delivery_relays/moq\u002dlite\u002d06/relays=2/publishers=16/broadcasts=1/viewers=64/watch=1/frame=64 time","culprit":null} -->

- Series: [`rust/relay/relay_session_delivery_relays/moq-lite-06/relays=2/publishers=16/broadcasts=1/viewers=64/watch=1/frame=64 time`](https://kixel.quest/factory/series/18759), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-26: 14.19 % against a baseline of 5 %, +9.188 % (+183.8%), at [`4fba5df`](https://github.com/moq-dev/moq/commit/4fba5df73f7ef73743a6ebdfdc1550a3dc83ed2a). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 0.00032.

### `rust/relay/relay_session_delivery_relays/moq-lite-06/relays=4/publishers=16/broadcasts=1/viewers=64/watch=1/frame=64 time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_delivery_relays/moq\u002dlite\u002d06/relays=4/publishers=16/broadcasts=1/viewers=64/watch=1/frame=64 time","culprit":null} -->

- Series: [`rust/relay/relay_session_delivery_relays/moq-lite-06/relays=4/publishers=16/broadcasts=1/viewers=64/watch=1/frame=64 time`](https://kixel.quest/factory/series/18760), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-26: 26.87 % against a baseline of 5 %, +21.87 % (+437.4%), at [`4fba5df`](https://github.com/moq-dev/moq/commit/4fba5df73f7ef73743a6ebdfdc1550a3dc83ed2a). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 2.9e-13.

### `rust/relay/relay_session_delivery_relays/moq-lite-07-wip/relays=2/publishers=16/broadcasts=1/viewers=64/watch=1/frame=64 time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_delivery_relays/moq\u002dlite\u002d07\u002dwip/relays=2/publishers=16/broadcasts=1/viewers=64/watch=1/frame=64 time","culprit":null} -->

- Series: [`rust/relay/relay_session_delivery_relays/moq-lite-07-wip/relays=2/publishers=16/broadcasts=1/viewers=64/watch=1/frame=64 time`](https://kixel.quest/factory/series/18763), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-26: 28.03 % against a baseline of 5 %, +23.03 % (+460.6%), at [`4fba5df`](https://github.com/moq-dev/moq/commit/4fba5df73f7ef73743a6ebdfdc1550a3dc83ed2a). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 2.2e-14.

### `rust/relay/relay_session_delivery_relays/moq-lite-07-wip/relays=8/publishers=16/broadcasts=1/viewers=64/watch=1/frame=64 time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_delivery_relays/moq\u002dlite\u002d07\u002dwip/relays=8/publishers=16/broadcasts=1/viewers=64/watch=1/frame=64 time","culprit":null} -->

- Series: [`rust/relay/relay_session_delivery_relays/moq-lite-07-wip/relays=8/publishers=16/broadcasts=1/viewers=64/watch=1/frame=64 time`](https://kixel.quest/factory/series/18765), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-26: 33.43 % against a baseline of 5 %, +28.43 % (+568.5%), at [`4fba5df`](https://github.com/moq-dev/moq/commit/4fba5df73f7ef73743a6ebdfdc1550a3dc83ed2a). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 3.9e-20.

### `rust/relay/relay_session_delivery_relays/moq-transport-22/relays=1/publishers=16/broadcasts=1/viewers=64/watch=1/frame=64 time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_delivery_relays/moq\u002dtransport\u002d22/relays=1/publishers=16/broadcasts=1/viewers=64/watch=1/frame=64 time","culprit":null} -->

- Series: [`rust/relay/relay_session_delivery_relays/moq-transport-22/relays=1/publishers=16/broadcasts=1/viewers=64/watch=1/frame=64 time`](https://kixel.quest/factory/series/18766), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-26: 19.67 % against a baseline of 5 %, +14.67 % (+293.5%), at [`4fba5df`](https://github.com/moq-dev/moq/commit/4fba5df73f7ef73743a6ebdfdc1550a3dc83ed2a). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 1.9e-7.

### `rust/relay/relay_session_delivery_relays/moq-transport-22/relays=2/publishers=16/broadcasts=1/viewers=64/watch=1/frame=64 time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_delivery_relays/moq\u002dtransport\u002d22/relays=2/publishers=16/broadcasts=1/viewers=64/watch=1/frame=64 time","culprit":null} -->

- Series: [`rust/relay/relay_session_delivery_relays/moq-transport-22/relays=2/publishers=16/broadcasts=1/viewers=64/watch=1/frame=64 time`](https://kixel.quest/factory/series/18767), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-26: 16.07 % against a baseline of 5 %, +11.07 % (+221.4%), at [`4fba5df`](https://github.com/moq-dev/moq/commit/4fba5df73f7ef73743a6ebdfdc1550a3dc83ed2a). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 0.000033.

### `rust/relay/relay_session_delivery_relays/moq-transport-22/relays=8/publishers=16/broadcasts=1/viewers=64/watch=1/frame=64 time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_delivery_relays/moq\u002dtransport\u002d22/relays=8/publishers=16/broadcasts=1/viewers=64/watch=1/frame=64 time","culprit":null} -->

- Series: [`rust/relay/relay_session_delivery_relays/moq-transport-22/relays=8/publishers=16/broadcasts=1/viewers=64/watch=1/frame=64 time`](https://kixel.quest/factory/series/18769), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-26: 25.36 % against a baseline of 5 %, +20.36 % (+407.2%), at [`4fba5df`](https://github.com/moq-dev/moq/commit/4fba5df73f7ef73743a6ebdfdc1550a3dc83ed2a). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 6.8e-12.

### `rust/relay/relay_session_delivery_scale/moq-lite-07-wip/relays=1/publishers=16/broadcasts=1/viewers=16/watch=1/frame=64 time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_delivery_scale/moq\u002dlite\u002d07\u002dwip/relays=1/publishers=16/broadcasts=1/viewers=16/watch=1/frame=64 time","culprit":null} -->

- Series: [`rust/relay/relay_session_delivery_scale/moq-lite-07-wip/relays=1/publishers=16/broadcasts=1/viewers=16/watch=1/frame=64 time`](https://kixel.quest/factory/series/18773), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-26: 15.39 % against a baseline of 5 %, +10.39 % (+207.7%), at [`4fba5df`](https://github.com/moq-dev/moq/commit/4fba5df73f7ef73743a6ebdfdc1550a3dc83ed2a). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 0.000077.

### `rust/relay/relay_session_delivery_scale/moq-transport-22/relays=1/publishers=64/broadcasts=1/viewers=64/watch=1/frame=64 time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_delivery_scale/moq\u002dtransport\u002d22/relays=1/publishers=64/broadcasts=1/viewers=64/watch=1/frame=64 time","culprit":null} -->

- Series: [`rust/relay/relay_session_delivery_scale/moq-transport-22/relays=1/publishers=64/broadcasts=1/viewers=64/watch=1/frame=64 time`](https://kixel.quest/factory/series/18778), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-26: 48.54 % against a baseline of 5 %, +43.54 % (+870.9%), at [`4fba5df`](https://github.com/moq-dev/moq/commit/4fba5df73f7ef73743a6ebdfdc1550a3dc83ed2a). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 1.2e-41.

### `rust/relay/relay_session_delivery_viewers/moq-lite-06/relays=1/publishers=16/broadcasts=1/viewers=256/watch=1/frame=64 time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_delivery_viewers/moq\u002dlite\u002d06/relays=1/publishers=16/broadcasts=1/viewers=256/watch=1/frame=64 time","culprit":null} -->

- Series: [`rust/relay/relay_session_delivery_viewers/moq-lite-06/relays=1/publishers=16/broadcasts=1/viewers=256/watch=1/frame=64 time`](https://kixel.quest/factory/series/18781), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-26: 11.74 % against a baseline of 5 %, +6.74 % (+134.8%), at [`4fba5df`](https://github.com/moq-dev/moq/commit/4fba5df73f7ef73743a6ebdfdc1550a3dc83ed2a). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 0.0040.

### `rust/relay/relay_session_delivery_watch/moq-lite-06/relays=1/publishers=1/broadcasts=256/viewers=16/watch=16/frame=64 time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_delivery_watch/moq\u002dlite\u002d06/relays=1/publishers=1/broadcasts=256/viewers=16/watch=16/frame=64 time","culprit":null} -->

- Series: [`rust/relay/relay_session_delivery_watch/moq-lite-06/relays=1/publishers=1/broadcasts=256/viewers=16/watch=16/frame=64 time`](https://kixel.quest/factory/series/18789), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-26: 18.69 % against a baseline of 5 %, +13.69 % (+273.8%), at [`4fba5df`](https://github.com/moq-dev/moq/commit/4fba5df73f7ef73743a6ebdfdc1550a3dc83ed2a). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 8.6e-7.

### `rust/relay/relay_session_delivery_watch/moq-lite-07-wip/relays=1/publishers=1/broadcasts=256/viewers=16/watch=1/frame=64 time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_delivery_watch/moq\u002dlite\u002d07\u002dwip/relays=1/publishers=1/broadcasts=256/viewers=16/watch=1/frame=64 time","culprit":null} -->

- Series: [`rust/relay/relay_session_delivery_watch/moq-lite-07-wip/relays=1/publishers=1/broadcasts=256/viewers=16/watch=1/frame=64 time`](https://kixel.quest/factory/series/18791), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-26: 11.44 % against a baseline of 5 %, +6.444 % (+128.9%), at [`4fba5df`](https://github.com/moq-dev/moq/commit/4fba5df73f7ef73743a6ebdfdc1550a3dc83ed2a). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 0.0053.

### `rust/relay/relay_session_delivery_watch/moq-lite-07-wip/relays=1/publishers=1/broadcasts=256/viewers=16/watch=16/frame=64 time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_delivery_watch/moq\u002dlite\u002d07\u002dwip/relays=1/publishers=1/broadcasts=256/viewers=16/watch=16/frame=64 time","culprit":null} -->

- Series: [`rust/relay/relay_session_delivery_watch/moq-lite-07-wip/relays=1/publishers=1/broadcasts=256/viewers=16/watch=16/frame=64 time`](https://kixel.quest/factory/series/18792), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-26: 20.05 % against a baseline of 5 %, +15.05 % (+301%), at [`4fba5df`](https://github.com/moq-dev/moq/commit/4fba5df73f7ef73743a6ebdfdc1550a3dc83ed2a). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 1.0e-7.

### `rust/relay/relay_session_join_stale/moq-lite-07-wip/relays=1/publishers=8/broadcasts=1/viewers=16/watch=1/frame=64/history=1024 time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_join_stale/moq\u002dlite\u002d07\u002dwip/relays=1/publishers=8/broadcasts=1/viewers=16/watch=1/frame=64/history=1024 time","culprit":null} -->

- Series: [`rust/relay/relay_session_join_stale/moq-lite-07-wip/relays=1/publishers=8/broadcasts=1/viewers=16/watch=1/frame=64/history=1024 time`](https://kixel.quest/factory/series/18850), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-26: 12.6 % against a baseline of 5 %, +7.598 % (+152%), at [`4fba5df`](https://github.com/moq-dev/moq/commit/4fba5df73f7ef73743a6ebdfdc1550a3dc83ed2a). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 0.0017.

### `rust/relay/relay_session_join_stale/moq-transport-22/relays=1/publishers=1/broadcasts=1/viewers=16/watch=1/frame=64/history=64 time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_join_stale/moq\u002dtransport\u002d22/relays=1/publishers=1/broadcasts=1/viewers=16/watch=1/frame=64/history=64 time","culprit":null} -->

- Series: [`rust/relay/relay_session_join_stale/moq-transport-22/relays=1/publishers=1/broadcasts=1/viewers=16/watch=1/frame=64/history=64 time`](https://kixel.quest/factory/series/18852), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-26: 11.51 % against a baseline of 5 %, +6.512 % (+130.2%), at [`4fba5df`](https://github.com/moq-dev/moq/commit/4fba5df73f7ef73743a6ebdfdc1550a3dc83ed2a). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 0.0050.

### `rust/relay/relay_session_withdrawal/16r_1b time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_withdrawal/16r_1b time","culprit":null} -->

- Series: [`rust/relay/relay_session_withdrawal/16r_1b time`](https://kixel.quest/factory/series/18860), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-26: 16.33 % against a baseline of 5 %, +11.33 % (+226.7%), at [`4fba5df`](https://github.com/moq-dev/moq/commit/4fba5df73f7ef73743a6ebdfdc1550a3dc83ed2a). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 0.000023.

### `rust/relay/relay_session_withdrawal/4r_32b time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_withdrawal/4r_32b time","culprit":null} -->

- Series: [`rust/relay/relay_session_withdrawal/4r_32b time`](https://kixel.quest/factory/series/18863), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-26: 11.6 % against a baseline of 5 %, +6.599 % (+132%), at [`4fba5df`](https://github.com/moq-dev/moq/commit/4fba5df73f7ef73743a6ebdfdc1550a3dc83ed2a). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 0.0046.

### `rust/relay/relay_session_withdrawal/8r_1b time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_withdrawal/8r_1b time","culprit":null} -->

- Series: [`rust/relay/relay_session_withdrawal/8r_1b time`](https://kixel.quest/factory/series/18864), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-26: 17.5 % against a baseline of 5 %, +12.5 % (+249.9%), at [`4fba5df`](https://github.com/moq-dev/moq/commit/4fba5df73f7ef73743a6ebdfdc1550a3dc83ed2a). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 0.0000048.

### `rust/relay/relay_session_withdrawal/8r_32b time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_withdrawal/8r_32b time","culprit":null} -->

- Series: [`rust/relay/relay_session_withdrawal/8r_32b time`](https://kixel.quest/factory/series/18865), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-26: 13.63 % against a baseline of 5 %, +8.627 % (+172.5%), at [`4fba5df`](https://github.com/moq-dev/moq/commit/4fba5df73f7ef73743a6ebdfdc1550a3dc83ed2a). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 0.00059.

### `rust/shared/catalog_write_order/1 time`: flake

<!-- kixel.quest finding {"series":"rust/shared/catalog_write_order/1 time","culprit":null} -->

- Series: [`rust/shared/catalog_write_order/1 time`](https://kixel.quest/factory/series/18899), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-26: 14.91 % against a baseline of 5 %, +9.909 % (+198.2%), at [`4fba5df`](https://github.com/moq-dev/moq/commit/4fba5df73f7ef73743a6ebdfdc1550a3dc83ed2a). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 0.00014.

### `rust/shared/cmaf-import-frames/1tracks-1frames/256 time`: flake

<!-- kixel.quest finding {"series":"rust/shared/cmaf\u002dimport\u002dframes/1tracks\u002d1frames/256 time","culprit":null} -->

- Series: [`rust/shared/cmaf-import-frames/1tracks-1frames/256 time`](https://kixel.quest/factory/series/18970), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-26: 51.15 % against a baseline of 5 %, +46.15 % (+922.9%), at [`4fba5df`](https://github.com/moq-dev/moq/commit/4fba5df73f7ef73743a6ebdfdc1550a3dc83ed2a). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 3.6e-46.

### `rust/shared/cmaf-unwrap-frames/1tracks-1frames/256 time`: flake

<!-- kixel.quest finding {"series":"rust/shared/cmaf\u002dunwrap\u002dframes/1tracks\u002d1frames/256 time","culprit":null} -->

- Series: [`rust/shared/cmaf-unwrap-frames/1tracks-1frames/256 time`](https://kixel.quest/factory/series/19049), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-26: 70.09 % against a baseline of 5 %, +65.09 % (+1302%), at [`4fba5df`](https://github.com/moq-dev/moq/commit/4fba5df73f7ef73743a6ebdfdc1550a3dc83ed2a). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 4.6e-86.

### `rust/shared/receive_reset/streams_1024/bytes_16384 time`: flake

<!-- kixel.quest finding {"series":"rust/shared/receive_reset/streams_1024/bytes_16384 time","culprit":null} -->

- Series: [`rust/shared/receive_reset/streams_1024/bytes_16384 time`](https://kixel.quest/factory/series/19316), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-26: 13.33 % against a baseline of 5 %, +8.331 % (+166.6%), at [`4fba5df`](https://github.com/moq-dev/moq/commit/4fba5df73f7ef73743a6ebdfdc1550a3dc83ed2a). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 0.00082.

### `rust/shared/relay_auth_refresh/16/16 time`: flake

<!-- kixel.quest finding {"series":"rust/shared/relay_auth_refresh/16/16 time","culprit":null} -->

- Series: [`rust/shared/relay_auth_refresh/16/16 time`](https://kixel.quest/factory/series/19450), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-26: 12.37 % against a baseline of 5 %, +7.372 % (+147.4%), at [`4fba5df`](https://github.com/moq-dev/moq/commit/4fba5df73f7ef73743a6ebdfdc1550a3dc83ed2a). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 0.0022.

### `rust/shared/relay_http_admission/connections=1/16 time`: flake

<!-- kixel.quest finding {"series":"rust/shared/relay_http_admission/connections=1/16 time","culprit":null} -->

- Series: [`rust/shared/relay_http_admission/connections=1/16 time`](https://kixel.quest/factory/series/19456), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-26: 11.75 % against a baseline of 5 %, +6.747 % (+134.9%), at [`4fba5df`](https://github.com/moq-dev/moq/commit/4fba5df73f7ef73743a6ebdfdc1550a3dc83ed2a). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 0.0040.

### `rust/shared/relay_http_admission/connections=16/16 time`: flake

<!-- kixel.quest finding {"series":"rust/shared/relay_http_admission/connections=16/16 time","culprit":null} -->

- Series: [`rust/shared/relay_http_admission/connections=16/16 time`](https://kixel.quest/factory/series/19459), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-26: 11.15 % against a baseline of 5 %, +6.149 % (+123%), at [`4fba5df`](https://github.com/moq-dev/moq/commit/4fba5df73f7ef73743a6ebdfdc1550a3dc83ed2a). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 0.0069.

### `rust/shared/relay_http_admission/connections=16/64 time`: flake

<!-- kixel.quest finding {"series":"rust/shared/relay_http_admission/connections=16/64 time","culprit":null} -->

- Series: [`rust/shared/relay_http_admission/connections=16/64 time`](https://kixel.quest/factory/series/19460), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-26: 11.65 % against a baseline of 5 %, +6.648 % (+133%), at [`4fba5df`](https://github.com/moq-dev/moq/commit/4fba5df73f7ef73743a6ebdfdc1550a3dc83ed2a). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 0.0044.

### `rust/shared/rtmp_amf0/nulls/1024 time`: flake

<!-- kixel.quest finding {"series":"rust/shared/rtmp_amf0/nulls/1024 time","culprit":null} -->

- Series: [`rust/shared/rtmp_amf0/nulls/1024 time`](https://kixel.quest/factory/series/19510), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-26: 14 % against a baseline of 5 %, +9.001 % (+180%), at [`4fba5df`](https://github.com/moq-dev/moq/commit/4fba5df73f7ef73743a6ebdfdc1550a3dc83ed2a). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 0.0051.

### `rust/shared/rtmp_amf0/nulls/1048576 time`: flake

<!-- kixel.quest finding {"series":"rust/shared/rtmp_amf0/nulls/1048576 time","culprit":null} -->

- Series: [`rust/shared/rtmp_amf0/nulls/1048576 time`](https://kixel.quest/factory/series/19511), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-26: 13.31 % against a baseline of 5 %, +8.308 % (+166.2%), at [`4fba5df`](https://github.com/moq-dev/moq/commit/4fba5df73f7ef73743a6ebdfdc1550a3dc83ed2a). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 0.0078.

### `rust/shared/rtmp_amf0/nulls/65536 time`: flake

<!-- kixel.quest finding {"series":"rust/shared/rtmp_amf0/nulls/65536 time","culprit":null} -->

- Series: [`rust/shared/rtmp_amf0/nulls/65536 time`](https://kixel.quest/factory/series/19512), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-26: 17.41 % against a baseline of 5 %, +12.41 % (+248.2%), at [`4fba5df`](https://github.com/moq-dev/moq/commit/4fba5df73f7ef73743a6ebdfdc1550a3dc83ed2a). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 0.00050.

### `rust/shared/schedule/group_priority/queued_4096_changed_16_tail time`: flake

<!-- kixel.quest finding {"series":"rust/shared/schedule/group_priority/queued_4096_changed_16_tail time","culprit":null} -->

- Series: [`rust/shared/schedule/group_priority/queued_4096_changed_16_tail time`](https://kixel.quest/factory/series/19360), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-26: 29.66 % against a baseline of 5 %, +24.66 % (+493.2%), at [`4fba5df`](https://github.com/moq-dev/moq/commit/4fba5df73f7ef73743a6ebdfdc1550a3dc83ed2a). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 5.2e-16.

### `rust/shared/schedule/group_priority/queued_4096_changed_256_tail time`: flake

<!-- kixel.quest finding {"series":"rust/shared/schedule/group_priority/queued_4096_changed_256_tail time","culprit":null} -->

- Series: [`rust/shared/schedule/group_priority/queued_4096_changed_256_tail time`](https://kixel.quest/factory/series/19363), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-26: 16.97 % against a baseline of 5 %, +11.97 % (+239.3%), at [`4fba5df`](https://github.com/moq-dev/moq/commit/4fba5df73f7ef73743a6ebdfdc1550a3dc83ed2a). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 0.000010.

### `rust/shared/stats/buckets/10000p_1000b time`: flake

<!-- kixel.quest finding {"series":"rust/shared/stats/buckets/10000p_1000b time","culprit":null} -->

- Series: [`rust/shared/stats/buckets/10000p_1000b time`](https://kixel.quest/factory/series/19549), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-26: 13.88 % against a baseline of 5 %, +8.877 % (+177.5%), at [`4fba5df`](https://github.com/moq-dev/moq/commit/4fba5df73f7ef73743a6ebdfdc1550a3dc83ed2a). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 0.00045.

### `rust/shared/stats/buckets/10000p_100b time`: flake

<!-- kixel.quest finding {"series":"rust/shared/stats/buckets/10000p_100b time","culprit":null} -->

- Series: [`rust/shared/stats/buckets/10000p_100b time`](https://kixel.quest/factory/series/19550), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-26: 17.56 % against a baseline of 5 %, +12.56 % (+251.3%), at [`4fba5df`](https://github.com/moq-dev/moq/commit/4fba5df73f7ef73743a6ebdfdc1550a3dc83ed2a). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 0.0000044.

### `rust/shared/stats/tick/1000p_4t_100pct time`: flake

<!-- kixel.quest finding {"series":"rust/shared/stats/tick/1000p_4t_100pct time","culprit":null} -->

- Series: [`rust/shared/stats/tick/1000p_4t_100pct time`](https://kixel.quest/factory/series/19588), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-26: 11.46 % against a baseline of 5 %, +6.458 % (+129.2%), at [`4fba5df`](https://github.com/moq-dev/moq/commit/4fba5df73f7ef73743a6ebdfdc1550a3dc83ed2a). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 0.0052.

### `rust/shared/ts_si_renewal_pcr/4096/1 time`: flake

<!-- kixel.quest finding {"series":"rust/shared/ts_si_renewal_pcr/4096/1 time","culprit":null} -->

- Series: [`rust/shared/ts_si_renewal_pcr/4096/1 time`](https://kixel.quest/factory/series/19084), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-26: 11.82 % against a baseline of 5 %, +6.819 % (+136.4%), at [`4fba5df`](https://github.com/moq-dev/moq/commit/4fba5df73f7ef73743a6ebdfdc1550a3dc83ed2a). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 0.0037.

### `rust/shared/ts_si_renewal_pcr/64/1 time`: flake

<!-- kixel.quest finding {"series":"rust/shared/ts_si_renewal_pcr/64/1 time","culprit":null} -->

- Series: [`rust/shared/ts_si_renewal_pcr/64/1 time`](https://kixel.quest/factory/series/19087), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-26: 14.22 % against a baseline of 5 %, +9.221 % (+184.4%), at [`4fba5df`](https://github.com/moq-dev/moq/commit/4fba5df73f7ef73743a6ebdfdc1550a3dc83ed2a). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 0.00031.

### `rust/shared/ts_si_table_history_pcr/1024/16 time`: flake

<!-- kixel.quest finding {"series":"rust/shared/ts_si_table_history_pcr/1024/16 time","culprit":null} -->

- Series: [`rust/shared/ts_si_table_history_pcr/1024/16 time`](https://kixel.quest/factory/series/19094), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-26: 16.95 % against a baseline of 5 %, +11.95 % (+239.1%), at [`4fba5df`](https://github.com/moq-dev/moq/commit/4fba5df73f7ef73743a6ebdfdc1550a3dc83ed2a). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 0.000010.

### `rust/shared/video-reorder/flat/64x256 time`: flake

<!-- kixel.quest finding {"series":"rust/shared/video\u002dreorder/flat/64x256 time","culprit":null} -->

- Series: [`rust/shared/video-reorder/flat/64x256 time`](https://kixel.quest/factory/series/19146), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-26: 14.52 % against a baseline of 5 %, +9.521 % (+190.4%), at [`4fba5df`](https://github.com/moq-dev/moq/commit/4fba5df73f7ef73743a6ebdfdc1550a3dc83ed2a). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 0.00022.

### `rust/shared/worker_request_reply/connections-1/bytes-262144 time`: flake

<!-- kixel.quest finding {"series":"rust/shared/worker_request_reply/connections\u002d1/bytes\u002d262144 time","culprit":null} -->

- Series: [`rust/shared/worker_request_reply/connections-1/bytes-262144 time`](https://kixel.quest/factory/series/19603), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-26: 16.09 % against a baseline of 5 %, +11.09 % (+221.7%), at [`4fba5df`](https://github.com/moq-dev/moq/commit/4fba5df73f7ef73743a6ebdfdc1550a3dc83ed2a). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 0.000032.

### `rust/shared/worker_request_reply/connections-1/bytes-64 time`: flake

<!-- kixel.quest finding {"series":"rust/shared/worker_request_reply/connections\u002d1/bytes\u002d64 time","culprit":null} -->

- Series: [`rust/shared/worker_request_reply/connections-1/bytes-64 time`](https://kixel.quest/factory/series/19604), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-26: 34 % against a baseline of 5 %, +29 % (+580%), at [`4fba5df`](https://github.com/moq-dev/moq/commit/4fba5df73f7ef73743a6ebdfdc1550a3dc83ed2a). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 8.3e-21.

### `rust/uring-udp_tokio/udp_tokio_epoll/recvmmsg_gro-on_gso-on time`: flake

<!-- kixel.quest finding {"series":"rust/uring\u002dudp_tokio/udp_tokio_epoll/recvmmsg_gro\u002don_gso\u002don time","culprit":null} -->

- Series: [`rust/uring-udp_tokio/udp_tokio_epoll/recvmmsg_gro-on_gso-on time`](https://kixel.quest/factory/series/19746), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-26: 40.3 % against a baseline of 5 %, +35.3 % (+705.9%), at [`4fba5df`](https://github.com/moq-dev/moq/commit/4fba5df73f7ef73743a6ebdfdc1550a3dc83ed2a). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 7.7e-16.

### `rust/uring-udp_tokio/udp_tokio_epoll/recvmsg_gro-on_gso-on time`: flake

<!-- kixel.quest finding {"series":"rust/uring\u002dudp_tokio/udp_tokio_epoll/recvmsg_gro\u002don_gso\u002don time","culprit":null} -->

- Series: [`rust/uring-udp_tokio/udp_tokio_epoll/recvmsg_gro-on_gso-on time`](https://kixel.quest/factory/series/19750), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-26: 42.18 % against a baseline of 5 %, +37.18 % (+743.6%), at [`4fba5df`](https://github.com/moq-dev/moq/commit/4fba5df73f7ef73743a6ebdfdc1550a3dc83ed2a). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 3.3e-17.

### `rust/uring-udp_uring/udp_io_uring/recvmsg-multishot_gro-on_gso-on time`: flake

<!-- kixel.quest finding {"series":"rust/uring\u002dudp_uring/udp_io_uring/recvmsg\u002dmultishot_gro\u002don_gso\u002don time","culprit":null} -->

- Series: [`rust/uring-udp_uring/udp_io_uring/recvmsg-multishot_gro-on_gso-on time`](https://kixel.quest/factory/series/19754), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-26: 24.23 % against a baseline of 5 %, +19.23 % (+384.6%), at [`4fba5df`](https://github.com/moq-dev/moq/commit/4fba5df73f7ef73743a6ebdfdc1550a3dc83ed2a). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 0.0000013.
