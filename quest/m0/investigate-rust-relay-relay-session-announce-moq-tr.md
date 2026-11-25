# [M] Investigate rust/relay/relay session announce/moq-transport-22/broadcasts=4096/viewers=1 time, rust/relay/relay…

## Goal

Kixel Quest measured what the Plan lists and filed it here on its own. For each finding:

1. Diagnose its cause, starting from the culprit's diff and sessions where it has them.
2. Have a second session, one that did not write the diagnosis, try to refute it, and diagnose again when it does.
3. Fix it, accept it with the reason recorded in this Plan, or improve on it further. A fix carries the measurement that shows it.

Then look past the finding: make the related improvements the diagnosis turns up where they fit here, and add the rest as follow-up quests.

## Plan

Each finding keeps the comment Kixel Quest finds it by, so a later run that sees it again adds its sighting here instead of filing it anew.

### `rust/relay/relay_session_announce/moq-transport-22/broadcasts=4096/viewers=1 time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_announce/moq\u002dtransport\u002d22/broadcasts=4096/viewers=1 time","culprit":null} -->

- Series: [`rust/relay/relay_session_announce/moq-transport-22/broadcasts=4096/viewers=1 time`](https://kixel.quest/factory/series/18454), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-25: 30.92 % against a baseline of 5 %, +25.92 % (+518.4%), at [`420b8dc`](https://github.com/moq-dev/moq/commit/420b8dcb0d9fc1693a90d0cabb6a77e88970c944). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 6.2e-10.

### `rust/relay/relay_session_announce/moq-transport-22/broadcasts=4096/viewers=64 time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_announce/moq\u002dtransport\u002d22/broadcasts=4096/viewers=64 time","culprit":null} -->

- Series: [`rust/relay/relay_session_announce/moq-transport-22/broadcasts=4096/viewers=64 time`](https://kixel.quest/factory/series/18455), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-25: 15.65 % against a baseline of 5 %, +10.65 % (+213.1%), at [`420b8dc`](https://github.com/moq-dev/moq/commit/420b8dcb0d9fc1693a90d0cabb6a77e88970c944). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 0.0017.

### `rust/relay/relay_session_churn_held/moq-lite-06/sessions=1/held=1 time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_churn_held/moq\u002dlite\u002d06/sessions=1/held=1 time","culprit":null} -->

- Series: [`rust/relay/relay_session_churn_held/moq-lite-06/sessions=1/held=1 time`](https://kixel.quest/factory/series/18459), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-25: 15.64 % against a baseline of 5 %, +10.64 % (+212.8%), at [`420b8dc`](https://github.com/moq-dev/moq/commit/420b8dcb0d9fc1693a90d0cabb6a77e88970c944). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 0.0018.

### `rust/relay/relay_session_churn_held/moq-lite-06/sessions=4/held=1 time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_churn_held/moq\u002dlite\u002d06/sessions=4/held=1 time","culprit":null} -->

- Series: [`rust/relay/relay_session_churn_held/moq-lite-06/sessions=4/held=1 time`](https://kixel.quest/factory/series/18462), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-25: 13.31 % against a baseline of 5 %, +8.313 % (+166.3%), at [`420b8dc`](https://github.com/moq-dev/moq/commit/420b8dcb0d9fc1693a90d0cabb6a77e88970c944). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 0.0078.

### `rust/relay/relay_session_churn_held/moq-lite-06/sessions=4/held=1024 time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_churn_held/moq\u002dlite\u002d06/sessions=4/held=1024 time","culprit":null} -->

- Series: [`rust/relay/relay_session_churn_held/moq-lite-06/sessions=4/held=1024 time`](https://kixel.quest/factory/series/18463), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-25: 24.2 % against a baseline of 5 %, +19.2 % (+384.1%), at [`420b8dc`](https://github.com/moq-dev/moq/commit/420b8dcb0d9fc1693a90d0cabb6a77e88970c944). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 0.0000013.

### `rust/relay/relay_session_churn_held/moq-lite-06/sessions=4/held=64 time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_churn_held/moq\u002dlite\u002d06/sessions=4/held=64 time","culprit":null} -->

- Series: [`rust/relay/relay_session_churn_held/moq-lite-06/sessions=4/held=64 time`](https://kixel.quest/factory/series/18464), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-25: 16.06 % against a baseline of 5 %, +11.06 % (+221.2%), at [`420b8dc`](https://github.com/moq-dev/moq/commit/420b8dcb0d9fc1693a90d0cabb6a77e88970c944). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 0.0013.

### `rust/relay/relay_session_churn_held/moq-transport-22/sessions=16/held=1 time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_churn_held/moq\u002dtransport\u002d22/sessions=16/held=1 time","culprit":null} -->

- Series: [`rust/relay/relay_session_churn_held/moq-transport-22/sessions=16/held=1 time`](https://kixel.quest/factory/series/18465), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-25: 13.35 % against a baseline of 5 %, +8.35 % (+167%), at [`420b8dc`](https://github.com/moq-dev/moq/commit/420b8dcb0d9fc1693a90d0cabb6a77e88970c944). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 0.0076.

### `rust/relay/relay_session_churn_held/moq-transport-22/sessions=16/held=1024 time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_churn_held/moq\u002dtransport\u002d22/sessions=16/held=1024 time","culprit":null} -->

- Series: [`rust/relay/relay_session_churn_held/moq-transport-22/sessions=16/held=1024 time`](https://kixel.quest/factory/series/18466), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-25: 14.95 % against a baseline of 5 %, +9.952 % (+199%), at [`420b8dc`](https://github.com/moq-dev/moq/commit/420b8dcb0d9fc1693a90d0cabb6a77e88970c944). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 0.0028.

### `rust/relay/relay_session_churn_sessions/moq-lite-06/sessions=128/held=16 time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_churn_sessions/moq\u002dlite\u002d06/sessions=128/held=16 time","culprit":null} -->

- Series: [`rust/relay/relay_session_churn_sessions/moq-lite-06/sessions=128/held=16 time`](https://kixel.quest/factory/series/18474), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-25: 14.94 % against a baseline of 5 %, +9.936 % (+198.7%), at [`420b8dc`](https://github.com/moq-dev/moq/commit/420b8dcb0d9fc1693a90d0cabb6a77e88970c944). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 0.0028.

### `rust/relay/relay_session_churn_sessions/moq-lite-06/sessions=16/held=16 time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_churn_sessions/moq\u002dlite\u002d06/sessions=16/held=16 time","culprit":null} -->

- Series: [`rust/relay/relay_session_churn_sessions/moq-lite-06/sessions=16/held=16 time`](https://kixel.quest/factory/series/18475), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-25: 14.3 % against a baseline of 5 %, +9.305 % (+186.1%), at [`420b8dc`](https://github.com/moq-dev/moq/commit/420b8dcb0d9fc1693a90d0cabb6a77e88970c944). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 0.0042.

### `rust/relay/relay_session_churn_sessions/moq-transport-22/sessions=128/held=16 time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_churn_sessions/moq\u002dtransport\u002d22/sessions=128/held=16 time","culprit":null} -->

- Series: [`rust/relay/relay_session_churn_sessions/moq-transport-22/sessions=128/held=16 time`](https://kixel.quest/factory/series/18477), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-25: 21.34 % against a baseline of 5 %, +16.34 % (+326.7%), at [`420b8dc`](https://github.com/moq-dev/moq/commit/420b8dcb0d9fc1693a90d0cabb6a77e88970c944). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 0.000020.

### `rust/relay/relay_session_dash_projects/moq-lite-06/relays=5/projects=1/tracks=24/total=96 time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_dash_projects/moq\u002dlite\u002d06/relays=5/projects=1/tracks=24/total=96 time","culprit":null} -->

- Series: [`rust/relay/relay_session_dash_projects/moq-lite-06/relays=5/projects=1/tracks=24/total=96 time`](https://kixel.quest/factory/series/18481), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-25: 22.42 % against a baseline of 5 %, +17.42 % (+348.3%), at [`420b8dc`](https://github.com/moq-dev/moq/commit/420b8dcb0d9fc1693a90d0cabb6a77e88970c944). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 0.0000073.

### `rust/relay/relay_session_dash_projects/moq-lite-06/relays=5/projects=16/tracks=24/total=1536 time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_dash_projects/moq\u002dlite\u002d06/relays=5/projects=16/tracks=24/total=1536 time","culprit":null} -->

- Series: [`rust/relay/relay_session_dash_projects/moq-lite-06/relays=5/projects=16/tracks=24/total=1536 time`](https://kixel.quest/factory/series/18480), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-25: 13 % against a baseline of 5 %, +8.004 % (+160.1%), at [`420b8dc`](https://github.com/moq-dev/moq/commit/420b8dcb0d9fc1693a90d0cabb6a77e88970c944). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 0.0093.

### `rust/relay/relay_session_dash_projects/moq-lite-06/relays=5/projects=4/tracks=24/total=384 time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_dash_projects/moq\u002dlite\u002d06/relays=5/projects=4/tracks=24/total=384 time","culprit":null} -->

- Series: [`rust/relay/relay_session_dash_projects/moq-lite-06/relays=5/projects=4/tracks=24/total=384 time`](https://kixel.quest/factory/series/18482), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-25: 14.48 % against a baseline of 5 %, +9.482 % (+189.6%), at [`420b8dc`](https://github.com/moq-dev/moq/commit/420b8dcb0d9fc1693a90d0cabb6a77e88970c944). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 0.0038.

### `rust/relay/relay_session_dash_projects/moq-transport-22/relays=5/projects=1/tracks=24/total=96 time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_dash_projects/moq\u002dtransport\u002d22/relays=5/projects=1/tracks=24/total=96 time","culprit":null} -->

- Series: [`rust/relay/relay_session_dash_projects/moq-transport-22/relays=5/projects=1/tracks=24/total=96 time`](https://kixel.quest/factory/series/18484), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-25: 14.37 % against a baseline of 5 %, +9.37 % (+187.4%), at [`420b8dc`](https://github.com/moq-dev/moq/commit/420b8dcb0d9fc1693a90d0cabb6a77e88970c944). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 0.0041.

### `rust/relay/relay_session_dash_projects/moq-transport-22/relays=5/projects=4/tracks=24/total=384 time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_dash_projects/moq\u002dtransport\u002d22/relays=5/projects=4/tracks=24/total=384 time","culprit":null} -->

- Series: [`rust/relay/relay_session_dash_projects/moq-transport-22/relays=5/projects=4/tracks=24/total=384 time`](https://kixel.quest/factory/series/18485), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-25: 17.13 % against a baseline of 5 %, +12.13 % (+242.5%), at [`420b8dc`](https://github.com/moq-dev/moq/commit/420b8dcb0d9fc1693a90d0cabb6a77e88970c944). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 0.00061.

### `rust/relay/relay_session_dash_relays/moq-lite-06/relays=3/projects=16/tracks=24/total=768 time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_dash_relays/moq\u002dlite\u002d06/relays=3/projects=16/tracks=24/total=768 time","culprit":null} -->

- Series: [`rust/relay/relay_session_dash_relays/moq-lite-06/relays=3/projects=16/tracks=24/total=768 time`](https://kixel.quest/factory/series/18490), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-25: 16.04 % against a baseline of 5 %, +11.04 % (+220.9%), at [`420b8dc`](https://github.com/moq-dev/moq/commit/420b8dcb0d9fc1693a90d0cabb6a77e88970c944). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 0.0013.

### `rust/relay/relay_session_dash_relays/moq-lite-06/relays=5/projects=8/tracks=24/total=768 time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_dash_relays/moq\u002dlite\u002d06/relays=5/projects=8/tracks=24/total=768 time","culprit":null} -->

- Series: [`rust/relay/relay_session_dash_relays/moq-lite-06/relays=5/projects=8/tracks=24/total=768 time`](https://kixel.quest/factory/series/18491), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-25: 15.3 % against a baseline of 5 %, +10.3 % (+206%), at [`420b8dc`](https://github.com/moq-dev/moq/commit/420b8dcb0d9fc1693a90d0cabb6a77e88970c944). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 0.0022.

### `rust/relay/relay_session_dash_relays/moq-transport-22/relays=17/projects=2/tracks=24/total=768 time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_dash_relays/moq\u002dtransport\u002d22/relays=17/projects=2/tracks=24/total=768 time","culprit":null} -->

- Series: [`rust/relay/relay_session_dash_relays/moq-transport-22/relays=17/projects=2/tracks=24/total=768 time`](https://kixel.quest/factory/series/18493), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-25: 14.44 % against a baseline of 5 %, +9.445 % (+188.9%), at [`420b8dc`](https://github.com/moq-dev/moq/commit/420b8dcb0d9fc1693a90d0cabb6a77e88970c944). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 0.0039.

### `rust/relay/relay_session_dash_relays/moq-transport-22/relays=2/projects=32/tracks=24/total=768 time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_dash_relays/moq\u002dtransport\u002d22/relays=2/projects=32/tracks=24/total=768 time","culprit":null} -->

- Series: [`rust/relay/relay_session_dash_relays/moq-transport-22/relays=2/projects=32/tracks=24/total=768 time`](https://kixel.quest/factory/series/18494), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-25: 14.46 % against a baseline of 5 %, +9.464 % (+189.3%), at [`420b8dc`](https://github.com/moq-dev/moq/commit/420b8dcb0d9fc1693a90d0cabb6a77e88970c944). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 0.0038.

### `rust/relay/relay_session_dash_relays/moq-transport-22/relays=5/projects=8/tracks=24/total=768 time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_dash_relays/moq\u002dtransport\u002d22/relays=5/projects=8/tracks=24/total=768 time","culprit":null} -->

- Series: [`rust/relay/relay_session_dash_relays/moq-transport-22/relays=5/projects=8/tracks=24/total=768 time`](https://kixel.quest/factory/series/18498), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-25: 18.86 % against a baseline of 5 %, +13.86 % (+277.2%), at [`420b8dc`](https://github.com/moq-dev/moq/commit/420b8dcb0d9fc1693a90d0cabb6a77e88970c944). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 0.00016.

### `rust/relay/relay_session_dash_relays/moq-transport-22/relays=9/projects=4/tracks=24/total=768 time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_dash_relays/moq\u002dtransport\u002d22/relays=9/projects=4/tracks=24/total=768 time","culprit":null} -->

- Series: [`rust/relay/relay_session_dash_relays/moq-transport-22/relays=9/projects=4/tracks=24/total=768 time`](https://kixel.quest/factory/series/18499), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-25: 14.27 % against a baseline of 5 %, +9.273 % (+185.5%), at [`420b8dc`](https://github.com/moq-dev/moq/commit/420b8dcb0d9fc1693a90d0cabb6a77e88970c944). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 0.0043.

### `rust/relay/relay_session_dash_tracks/moq-lite-06/relays=5/projects=4/tracks=24/total=384 time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_dash_tracks/moq\u002dlite\u002d06/relays=5/projects=4/tracks=24/total=384 time","culprit":null} -->

- Series: [`rust/relay/relay_session_dash_tracks/moq-lite-06/relays=5/projects=4/tracks=24/total=384 time`](https://kixel.quest/factory/series/18500), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-25: 22.64 % against a baseline of 5 %, +17.64 % (+352.7%), at [`420b8dc`](https://github.com/moq-dev/moq/commit/420b8dcb0d9fc1693a90d0cabb6a77e88970c944). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 0.0000060.

### `rust/relay/relay_session_dash_tracks/moq-lite-06/relays=5/projects=4/tracks=6/total=96 time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_dash_tracks/moq\u002dlite\u002d06/relays=5/projects=4/tracks=6/total=96 time","culprit":null} -->

- Series: [`rust/relay/relay_session_dash_tracks/moq-lite-06/relays=5/projects=4/tracks=6/total=96 time`](https://kixel.quest/factory/series/18501), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-25: 25.79 % against a baseline of 5 %, +20.79 % (+415.9%), at [`420b8dc`](https://github.com/moq-dev/moq/commit/420b8dcb0d9fc1693a90d0cabb6a77e88970c944). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 2.5e-7.

### `rust/relay/relay_session_dash_tracks/moq-transport-22/relays=5/projects=4/tracks=24/total=384 time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_dash_tracks/moq\u002dtransport\u002d22/relays=5/projects=4/tracks=24/total=384 time","culprit":null} -->

- Series: [`rust/relay/relay_session_dash_tracks/moq-transport-22/relays=5/projects=4/tracks=24/total=384 time`](https://kixel.quest/factory/series/18503), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-25: 27.68 % against a baseline of 5 %, +22.68 % (+453.7%), at [`420b8dc`](https://github.com/moq-dev/moq/commit/420b8dcb0d9fc1693a90d0cabb6a77e88970c944). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 3.1e-8.

### `rust/relay/relay_session_dash_tracks/moq-transport-22/relays=5/projects=4/tracks=6/total=96 time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_dash_tracks/moq\u002dtransport\u002d22/relays=5/projects=4/tracks=6/total=96 time","culprit":null} -->

- Series: [`rust/relay/relay_session_dash_tracks/moq-transport-22/relays=5/projects=4/tracks=6/total=96 time`](https://kixel.quest/factory/series/18504), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-25: 49.06 % against a baseline of 5 %, +44.06 % (+881.1%), at [`420b8dc`](https://github.com/moq-dev/moq/commit/420b8dcb0d9fc1693a90d0cabb6a77e88970c944). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 1.0e-22.

### `rust/relay/relay_session_delivery_broadcasts/moq-lite-07-wip/relays=1/publishers=1/broadcasts=4096/viewers=16/watch=1/frame=64 time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_delivery_broadcasts/moq\u002dlite\u002d07\u002dwip/relays=1/publishers=1/broadcasts=4096/viewers=16/watch=1/frame=64 time","culprit":null} -->

- Series: [`rust/relay/relay_session_delivery_broadcasts/moq-lite-07-wip/relays=1/publishers=1/broadcasts=4096/viewers=16/watch=1/frame=64 time`](https://kixel.quest/factory/series/18511), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-25: 15.4 % against a baseline of 5 %, +10.4 % (+207.9%), at [`420b8dc`](https://github.com/moq-dev/moq/commit/420b8dcb0d9fc1693a90d0cabb6a77e88970c944). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 0.0021.

### `rust/relay/relay_session_delivery_compression/moq-lite-07-wip/relays=1/publishers=16/broadcasts=1/viewers=16/watch=1/frame=1024 time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_delivery_compression/moq\u002dlite\u002d07\u002dwip/relays=1/publishers=16/broadcasts=1/viewers=16/watch=1/frame=1024 time","culprit":null} -->

- Series: [`rust/relay/relay_session_delivery_compression/moq-lite-07-wip/relays=1/publishers=16/broadcasts=1/viewers=16/watch=1/frame=1024 time`](https://kixel.quest/factory/series/18540), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-25: 13.16 % against a baseline of 5 %, +8.156 % (+163.1%), at [`420b8dc`](https://github.com/moq-dev/moq/commit/420b8dcb0d9fc1693a90d0cabb6a77e88970c944). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 0.0085.

### `rust/relay/relay_session_delivery_credit/moq-transport-22/watch=16/burst=64/credit=1 time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_delivery_credit/moq\u002dtransport\u002d22/watch=16/burst=64/credit=1 time","culprit":null} -->

- Series: [`rust/relay/relay_session_delivery_credit/moq-transport-22/watch=16/burst=64/credit=1 time`](https://kixel.quest/factory/series/18568), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-25: 13.81 % against a baseline of 5 %, +8.807 % (+176.1%), at [`420b8dc`](https://github.com/moq-dev/moq/commit/420b8dcb0d9fc1693a90d0cabb6a77e88970c944). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 0.0058.

### `rust/relay/relay_session_delivery_credit/moq-transport-22/watch=64/burst=1/credit=1 time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_delivery_credit/moq\u002dtransport\u002d22/watch=64/burst=1/credit=1 time","culprit":null} -->

- Series: [`rust/relay/relay_session_delivery_credit/moq-transport-22/watch=64/burst=1/credit=1 time`](https://kixel.quest/factory/series/18573), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-25: 19.88 % against a baseline of 5 %, +14.88 % (+297.6%), at [`420b8dc`](https://github.com/moq-dev/moq/commit/420b8dcb0d9fc1693a90d0cabb6a77e88970c944). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 0.000070.

### `rust/relay/relay_session_delivery_datagram/moq-lite-06/publishers=1/viewers=64/payload=64/cadence=1 time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_delivery_datagram/moq\u002dlite\u002d06/publishers=1/viewers=64/payload=64/cadence=1 time","culprit":null} -->

- Series: [`rust/relay/relay_session_delivery_datagram/moq-lite-06/publishers=1/viewers=64/payload=64/cadence=1 time`](https://kixel.quest/factory/series/18583), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-25: 17.31 % against a baseline of 5 %, +12.31 % (+246.2%), at [`420b8dc`](https://github.com/moq-dev/moq/commit/420b8dcb0d9fc1693a90d0cabb6a77e88970c944). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 0.00054.

### `rust/relay/relay_session_delivery_datagram/moq-lite-06/publishers=16/viewers=64/payload=64/cadence=1 time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_delivery_datagram/moq\u002dlite\u002d06/publishers=16/viewers=64/payload=64/cadence=1 time","culprit":null} -->

- Series: [`rust/relay/relay_session_delivery_datagram/moq-lite-06/publishers=16/viewers=64/payload=64/cadence=1 time`](https://kixel.quest/factory/series/18580), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-25: 14.82 % against a baseline of 5 %, +9.822 % (+196.4%), at [`420b8dc`](https://github.com/moq-dev/moq/commit/420b8dcb0d9fc1693a90d0cabb6a77e88970c944). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 0.0030.

### `rust/relay/relay_session_delivery_datagram/moq-lite-07-wip/publishers=1/viewers=16/payload=64/cadence=1 time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_delivery_datagram/moq\u002dlite\u002d07\u002dwip/publishers=1/viewers=16/payload=64/cadence=1 time","culprit":null} -->

- Series: [`rust/relay/relay_session_delivery_datagram/moq-lite-07-wip/publishers=1/viewers=16/payload=64/cadence=1 time`](https://kixel.quest/factory/series/18590), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-25: 14.8 % against a baseline of 5 %, +9.798 % (+196%), at [`420b8dc`](https://github.com/moq-dev/moq/commit/420b8dcb0d9fc1693a90d0cabb6a77e88970c944). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 0.0031.

### `rust/relay/relay_session_delivery_fetch_frames/moq-lite-06/relays=1/publishers=4/broadcasts=1/viewers=16/watch=1/frame=64/fetch time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_delivery_fetch_frames/moq\u002dlite\u002d06/relays=1/publishers=4/broadcasts=1/viewers=16/watch=1/frame=64/fetch time","culprit":null} -->

- Series: [`rust/relay/relay_session_delivery_fetch_frames/moq-lite-06/relays=1/publishers=4/broadcasts=1/viewers=16/watch=1/frame=64/fetch time`](https://kixel.quest/factory/series/18606), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-25: 16.31 % against a baseline of 5 %, +11.31 % (+226.1%), at [`420b8dc`](https://github.com/moq-dev/moq/commit/420b8dcb0d9fc1693a90d0cabb6a77e88970c944). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 0.0011.

### `rust/relay/relay_session_delivery_fetch_range/moq-transport-22/publishers=1/viewers=16/burst=2/fetch time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_delivery_fetch_range/moq\u002dtransport\u002d22/publishers=1/viewers=16/burst=2/fetch time","culprit":null} -->

- Series: [`rust/relay/relay_session_delivery_fetch_range/moq-transport-22/publishers=1/viewers=16/burst=2/fetch time`](https://kixel.quest/factory/series/18647), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-25: 13.31 % against a baseline of 5 %, +8.31 % (+166.2%), at [`420b8dc`](https://github.com/moq-dev/moq/commit/420b8dcb0d9fc1693a90d0cabb6a77e88970c944). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 0.0078.

### `rust/relay/relay_session_delivery_group_cost/moq-lite-06/relays=1/publishers=16/broadcasts=1/viewers=64/watch=1/frame=64 time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_delivery_group_cost/moq\u002dlite\u002d06/relays=1/publishers=16/broadcasts=1/viewers=64/watch=1/frame=64 time","culprit":null} -->

- Series: [`rust/relay/relay_session_delivery_group_cost/moq-lite-06/relays=1/publishers=16/broadcasts=1/viewers=64/watch=1/frame=64 time`](https://kixel.quest/factory/series/18669), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-25: 13.01 % against a baseline of 5 %, +8.007 % (+160.1%), at [`420b8dc`](https://github.com/moq-dev/moq/commit/420b8dcb0d9fc1693a90d0cabb6a77e88970c944). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 0.0093.

### `rust/relay/relay_session_delivery_group_cost/moq-lite-07-wip/relays=1/publishers=16/broadcasts=1/viewers=1/watch=1/frame=64 time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_delivery_group_cost/moq\u002dlite\u002d07\u002dwip/relays=1/publishers=16/broadcasts=1/viewers=1/watch=1/frame=64 time","culprit":null} -->

- Series: [`rust/relay/relay_session_delivery_group_cost/moq-lite-07-wip/relays=1/publishers=16/broadcasts=1/viewers=1/watch=1/frame=64 time`](https://kixel.quest/factory/series/18689), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-25: 16.8 % against a baseline of 5 %, +11.8 % (+236%), at [`420b8dc`](https://github.com/moq-dev/moq/commit/420b8dcb0d9fc1693a90d0cabb6a77e88970c944). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 0.00078.

### `rust/relay/relay_session_delivery_group_cost/moq-lite-07-wip/relays=1/publishers=16/broadcasts=1/viewers=64/watch=1/frame=64/fetch time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_delivery_group_cost/moq\u002dlite\u002d07\u002dwip/relays=1/publishers=16/broadcasts=1/viewers=64/watch=1/frame=64/fetch time","culprit":null} -->

- Series: [`rust/relay/relay_session_delivery_group_cost/moq-lite-07-wip/relays=1/publishers=16/broadcasts=1/viewers=64/watch=1/frame=64/fetch time`](https://kixel.quest/factory/series/18694), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-25: 23.16 % against a baseline of 5 %, +18.16 % (+363.3%), at [`420b8dc`](https://github.com/moq-dev/moq/commit/420b8dcb0d9fc1693a90d0cabb6a77e88970c944). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 0.0000036.

### `rust/relay/relay_session_delivery_group_cost/moq-transport-22/relays=1/publishers=1/broadcasts=1/viewers=1/watch=1/frame=64/fetch time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_delivery_group_cost/moq\u002dtransport\u002d22/relays=1/publishers=1/broadcasts=1/viewers=1/watch=1/frame=64/fetch time","culprit":null} -->

- Series: [`rust/relay/relay_session_delivery_group_cost/moq-transport-22/relays=1/publishers=1/broadcasts=1/viewers=1/watch=1/frame=64/fetch time`](https://kixel.quest/factory/series/18726), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-25: 13.92 % against a baseline of 5 %, +8.923 % (+178.5%), at [`420b8dc`](https://github.com/moq-dev/moq/commit/420b8dcb0d9fc1693a90d0cabb6a77e88970c944). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 0.0054.

### `rust/relay/relay_session_delivery_group_cost/moq-transport-22/relays=1/publishers=1/broadcasts=1/viewers=64/watch=1/frame=64/fetch time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_delivery_group_cost/moq\u002dtransport\u002d22/relays=1/publishers=1/broadcasts=1/viewers=64/watch=1/frame=64/fetch time","culprit":null} -->

- Series: [`rust/relay/relay_session_delivery_group_cost/moq-transport-22/relays=1/publishers=1/broadcasts=1/viewers=64/watch=1/frame=64/fetch time`](https://kixel.quest/factory/series/18730), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-25: 19.76 % against a baseline of 5 %, +14.76 % (+295.1%), at [`420b8dc`](https://github.com/moq-dev/moq/commit/420b8dcb0d9fc1693a90d0cabb6a77e88970c944). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 0.000078.

### `rust/relay/relay_session_delivery_group_cost/moq-transport-22/relays=1/publishers=16/broadcasts=1/viewers=16/watch=1/frame=64/window=64 time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_delivery_group_cost/moq\u002dtransport\u002d22/relays=1/publishers=16/broadcasts=1/viewers=16/watch=1/frame=64/window=64 time","culprit":null} -->

- Series: [`rust/relay/relay_session_delivery_group_cost/moq-transport-22/relays=1/publishers=16/broadcasts=1/viewers=16/watch=1/frame=64/window=64 time`](https://kixel.quest/factory/series/18707), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-25: 13.19 % against a baseline of 5 %, +8.193 % (+163.9%), at [`420b8dc`](https://github.com/moq-dev/moq/commit/420b8dcb0d9fc1693a90d0cabb6a77e88970c944). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 0.0083.

### `rust/relay/relay_session_delivery_group_cost/moq-transport-22/relays=1/publishers=16/broadcasts=1/viewers=64/watch=1/frame=64 time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_delivery_group_cost/moq\u002dtransport\u002d22/relays=1/publishers=16/broadcasts=1/viewers=64/watch=1/frame=64 time","culprit":null} -->

- Series: [`rust/relay/relay_session_delivery_group_cost/moq-transport-22/relays=1/publishers=16/broadcasts=1/viewers=64/watch=1/frame=64 time`](https://kixel.quest/factory/series/18717), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-25: 14.12 % against a baseline of 5 %, +9.118 % (+182.4%), at [`420b8dc`](https://github.com/moq-dev/moq/commit/420b8dcb0d9fc1693a90d0cabb6a77e88970c944). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 0.0047.

### `rust/relay/relay_session_delivery_group_cost/moq-transport-22/relays=1/publishers=16/broadcasts=1/viewers=64/watch=1/frame=64/window=64/fetch time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_delivery_group_cost/moq\u002dtransport\u002d22/relays=1/publishers=16/broadcasts=1/viewers=64/watch=1/frame=64/window=64/fetch time","culprit":null} -->

- Series: [`rust/relay/relay_session_delivery_group_cost/moq-transport-22/relays=1/publishers=16/broadcasts=1/viewers=64/watch=1/frame=64/window=64/fetch time`](https://kixel.quest/factory/series/18716), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-25: 14.41 % against a baseline of 5 %, +9.412 % (+188.2%), at [`420b8dc`](https://github.com/moq-dev/moq/commit/420b8dcb0d9fc1693a90d0cabb6a77e88970c944). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 0.0039.

### `rust/relay/relay_session_delivery_paced/moq-lite-07-wip/relays=1/publishers=16/broadcasts=1/viewers=16/watch=1/frame=64/paced time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_delivery_paced/moq\u002dlite\u002d07\u002dwip/relays=1/publishers=16/broadcasts=1/viewers=16/watch=1/frame=64/paced time","culprit":null} -->

- Series: [`rust/relay/relay_session_delivery_paced/moq-lite-07-wip/relays=1/publishers=16/broadcasts=1/viewers=16/watch=1/frame=64/paced time`](https://kixel.quest/factory/series/18734), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-25: 13.54 % against a baseline of 5 %, +8.54 % (+170.8%), at [`420b8dc`](https://github.com/moq-dev/moq/commit/420b8dcb0d9fc1693a90d0cabb6a77e88970c944). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 0.0068.

### `rust/relay/relay_session_delivery_paced/moq-lite-07-wip/relays=1/publishers=16/broadcasts=1/viewers=256/watch=1/frame=64/paced time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_delivery_paced/moq\u002dlite\u002d07\u002dwip/relays=1/publishers=16/broadcasts=1/viewers=256/watch=1/frame=64/paced time","culprit":null} -->

- Series: [`rust/relay/relay_session_delivery_paced/moq-lite-07-wip/relays=1/publishers=16/broadcasts=1/viewers=256/watch=1/frame=64/paced time`](https://kixel.quest/factory/series/18736), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-25: 15.22 % against a baseline of 5 %, +10.22 % (+204.5%), at [`420b8dc`](https://github.com/moq-dev/moq/commit/420b8dcb0d9fc1693a90d0cabb6a77e88970c944). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 0.0023.

### `rust/relay/relay_session_delivery_paced/moq-transport-22/relays=1/publishers=16/broadcasts=1/viewers=256/watch=1/frame=64/paced time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_delivery_paced/moq\u002dtransport\u002d22/relays=1/publishers=16/broadcasts=1/viewers=256/watch=1/frame=64/paced time","culprit":null} -->

- Series: [`rust/relay/relay_session_delivery_paced/moq-transport-22/relays=1/publishers=16/broadcasts=1/viewers=256/watch=1/frame=64/paced time`](https://kixel.quest/factory/series/18739), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-25: 15.04 % against a baseline of 5 %, +10.04 % (+200.9%), at [`420b8dc`](https://github.com/moq-dev/moq/commit/420b8dcb0d9fc1693a90d0cabb6a77e88970c944). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 0.0026.

### `rust/relay/relay_session_delivery_prefixes/moq-lite-06/broadcasts=1025/follow=64 time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_delivery_prefixes/moq\u002dlite\u002d06/broadcasts=1025/follow=64 time","culprit":null} -->

- Series: [`rust/relay/relay_session_delivery_prefixes/moq-lite-06/broadcasts=1025/follow=64 time`](https://kixel.quest/factory/series/18742), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-25: 20.07 % against a baseline of 5 %, +15.07 % (+301.5%), at [`420b8dc`](https://github.com/moq-dev/moq/commit/420b8dcb0d9fc1693a90d0cabb6a77e88970c944). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 0.000059.

### `rust/relay/relay_session_delivery_prefixes/moq-lite-07-wip/broadcasts=1025/follow=1024 time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_delivery_prefixes/moq\u002dlite\u002d07\u002dwip/broadcasts=1025/follow=1024 time","culprit":null} -->

- Series: [`rust/relay/relay_session_delivery_prefixes/moq-lite-07-wip/broadcasts=1025/follow=1024 time`](https://kixel.quest/factory/series/18744), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-25: 22.19 % against a baseline of 5 %, +17.19 % (+343.9%), at [`420b8dc`](https://github.com/moq-dev/moq/commit/420b8dcb0d9fc1693a90d0cabb6a77e88970c944). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 0.0000090.

### `rust/relay/relay_session_delivery_relays/moq-lite-06/relays=1/publishers=16/broadcasts=1/viewers=64/watch=1/frame=64 time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_delivery_relays/moq\u002dlite\u002d06/relays=1/publishers=16/broadcasts=1/viewers=64/watch=1/frame=64 time","culprit":null} -->

- Series: [`rust/relay/relay_session_delivery_relays/moq-lite-06/relays=1/publishers=16/broadcasts=1/viewers=64/watch=1/frame=64 time`](https://kixel.quest/factory/series/18758), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-25: 13.07 % against a baseline of 5 %, +8.073 % (+161.5%), at [`420b8dc`](https://github.com/moq-dev/moq/commit/420b8dcb0d9fc1693a90d0cabb6a77e88970c944). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 0.0089.

### `rust/relay/relay_session_delivery_relays/moq-lite-06/relays=8/publishers=16/broadcasts=1/viewers=64/watch=1/frame=64 time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_delivery_relays/moq\u002dlite\u002d06/relays=8/publishers=16/broadcasts=1/viewers=64/watch=1/frame=64 time","culprit":null} -->

- Series: [`rust/relay/relay_session_delivery_relays/moq-lite-06/relays=8/publishers=16/broadcasts=1/viewers=64/watch=1/frame=64 time`](https://kixel.quest/factory/series/18761), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-25: 23.98 % against a baseline of 5 %, +18.98 % (+379.7%), at [`420b8dc`](https://github.com/moq-dev/moq/commit/420b8dcb0d9fc1693a90d0cabb6a77e88970c944). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 0.0000016.

### `rust/relay/relay_session_delivery_relays/moq-lite-07-wip/relays=1/publishers=16/broadcasts=1/viewers=64/watch=1/frame=64 time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_delivery_relays/moq\u002dlite\u002d07\u002dwip/relays=1/publishers=16/broadcasts=1/viewers=64/watch=1/frame=64 time","culprit":null} -->

- Series: [`rust/relay/relay_session_delivery_relays/moq-lite-07-wip/relays=1/publishers=16/broadcasts=1/viewers=64/watch=1/frame=64 time`](https://kixel.quest/factory/series/18762), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-25: 15.08 % against a baseline of 5 %, +10.08 % (+201.6%), at [`420b8dc`](https://github.com/moq-dev/moq/commit/420b8dcb0d9fc1693a90d0cabb6a77e88970c944). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 0.0026.

### `rust/relay/relay_session_delivery_relays/moq-lite-07-wip/relays=4/publishers=16/broadcasts=1/viewers=64/watch=1/frame=64 time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_delivery_relays/moq\u002dlite\u002d07\u002dwip/relays=4/publishers=16/broadcasts=1/viewers=64/watch=1/frame=64 time","culprit":null} -->

- Series: [`rust/relay/relay_session_delivery_relays/moq-lite-07-wip/relays=4/publishers=16/broadcasts=1/viewers=64/watch=1/frame=64 time`](https://kixel.quest/factory/series/18764), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-25: 16.72 % against a baseline of 5 %, +11.72 % (+234.3%), at [`420b8dc`](https://github.com/moq-dev/moq/commit/420b8dcb0d9fc1693a90d0cabb6a77e88970c944). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 0.00083.

### `rust/relay/relay_session_delivery_relays/moq-transport-22/relays=4/publishers=16/broadcasts=1/viewers=64/watch=1/frame=64 time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_delivery_relays/moq\u002dtransport\u002d22/relays=4/publishers=16/broadcasts=1/viewers=64/watch=1/frame=64 time","culprit":null} -->

- Series: [`rust/relay/relay_session_delivery_relays/moq-transport-22/relays=4/publishers=16/broadcasts=1/viewers=64/watch=1/frame=64 time`](https://kixel.quest/factory/series/18768), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-25: 13.62 % against a baseline of 5 %, +8.622 % (+172.4%), at [`420b8dc`](https://github.com/moq-dev/moq/commit/420b8dcb0d9fc1693a90d0cabb6a77e88970c944). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 0.0064.

### `rust/relay/relay_session_delivery_scale/moq-lite-06/relays=1/publishers=64/broadcasts=1/viewers=64/watch=1/frame=64 time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_delivery_scale/moq\u002dlite\u002d06/relays=1/publishers=64/broadcasts=1/viewers=64/watch=1/frame=64 time","culprit":null} -->

- Series: [`rust/relay/relay_session_delivery_scale/moq-lite-06/relays=1/publishers=64/broadcasts=1/viewers=64/watch=1/frame=64 time`](https://kixel.quest/factory/series/18772), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-25: 14.92 % against a baseline of 5 %, +9.918 % (+198.4%), at [`420b8dc`](https://github.com/moq-dev/moq/commit/420b8dcb0d9fc1693a90d0cabb6a77e88970c944). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 0.0028.

### `rust/relay/relay_session_delivery_scale/moq-lite-07-wip/relays=1/publishers=64/broadcasts=1/viewers=64/watch=1/frame=64 time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_delivery_scale/moq\u002dlite\u002d07\u002dwip/relays=1/publishers=64/broadcasts=1/viewers=64/watch=1/frame=64 time","culprit":null} -->

- Series: [`rust/relay/relay_session_delivery_scale/moq-lite-07-wip/relays=1/publishers=64/broadcasts=1/viewers=64/watch=1/frame=64 time`](https://kixel.quest/factory/series/18775), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-25: 19.48 % against a baseline of 5 %, +14.48 % (+289.6%), at [`420b8dc`](https://github.com/moq-dev/moq/commit/420b8dcb0d9fc1693a90d0cabb6a77e88970c944). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 0.000098.

### `rust/relay/relay_session_delivery_viewers/moq-lite-07-wip/relays=1/publishers=16/broadcasts=1/viewers=16/watch=1/frame=64 time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_delivery_viewers/moq\u002dlite\u002d07\u002dwip/relays=1/publishers=16/broadcasts=1/viewers=16/watch=1/frame=64 time","culprit":null} -->

- Series: [`rust/relay/relay_session_delivery_viewers/moq-lite-07-wip/relays=1/publishers=16/broadcasts=1/viewers=16/watch=1/frame=64 time`](https://kixel.quest/factory/series/18782), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-25: 13.99 % against a baseline of 5 %, +8.991 % (+179.8%), at [`420b8dc`](https://github.com/moq-dev/moq/commit/420b8dcb0d9fc1693a90d0cabb6a77e88970c944). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 0.0051.

### `rust/relay/relay_session_join_broadcasts/moq-lite-06/relays=1/publishers=1/broadcasts=1/viewers=16/watch=1/frame=64 time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_join_broadcasts/moq\u002dlite\u002d06/relays=1/publishers=1/broadcasts=1/viewers=16/watch=1/frame=64 time","culprit":null} -->

- Series: [`rust/relay/relay_session_join_broadcasts/moq-lite-06/relays=1/publishers=1/broadcasts=1/viewers=16/watch=1/frame=64 time`](https://kixel.quest/factory/series/18798), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-25: 14.24 % against a baseline of 5 %, +9.235 % (+184.7%), at [`420b8dc`](https://github.com/moq-dev/moq/commit/420b8dcb0d9fc1693a90d0cabb6a77e88970c944). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 0.0044.

### `rust/relay/relay_session_join_broadcasts/moq-lite-06/relays=1/publishers=1/broadcasts=1024/viewers=16/watch=1/frame=64 time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_join_broadcasts/moq\u002dlite\u002d06/relays=1/publishers=1/broadcasts=1024/viewers=16/watch=1/frame=64 time","culprit":null} -->

- Series: [`rust/relay/relay_session_join_broadcasts/moq-lite-06/relays=1/publishers=1/broadcasts=1024/viewers=16/watch=1/frame=64 time`](https://kixel.quest/factory/series/18797), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-25: 19.85 % against a baseline of 5 %, +14.85 % (+297%), at [`420b8dc`](https://github.com/moq-dev/moq/commit/420b8dcb0d9fc1693a90d0cabb6a77e88970c944). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 0.000072.

### `rust/relay/relay_session_join_broadcasts/moq-lite-06/relays=2/publishers=1/broadcasts=1024/viewers=16/watch=1/frame=64 time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_join_broadcasts/moq\u002dlite\u002d06/relays=2/publishers=1/broadcasts=1024/viewers=16/watch=1/frame=64 time","culprit":null} -->

- Series: [`rust/relay/relay_session_join_broadcasts/moq-lite-06/relays=2/publishers=1/broadcasts=1024/viewers=16/watch=1/frame=64 time`](https://kixel.quest/factory/series/18800), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-25: 14.85 % against a baseline of 5 %, +9.854 % (+197.1%), at [`420b8dc`](https://github.com/moq-dev/moq/commit/420b8dcb0d9fc1693a90d0cabb6a77e88970c944). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 0.0030.

### `rust/relay/relay_session_join_broadcasts/moq-lite-07-wip/relays=1/publishers=1/broadcasts=1/viewers=16/watch=1/frame=64 time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_join_broadcasts/moq\u002dlite\u002d07\u002dwip/relays=1/publishers=1/broadcasts=1/viewers=16/watch=1/frame=64 time","culprit":null} -->

- Series: [`rust/relay/relay_session_join_broadcasts/moq-lite-07-wip/relays=1/publishers=1/broadcasts=1/viewers=16/watch=1/frame=64 time`](https://kixel.quest/factory/series/18807), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-25: 13.19 % against a baseline of 5 %, +8.19 % (+163.8%), at [`420b8dc`](https://github.com/moq-dev/moq/commit/420b8dcb0d9fc1693a90d0cabb6a77e88970c944). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 0.0083.

### `rust/relay/relay_session_join_broadcasts/moq-transport-22/relays=1/publishers=1/broadcasts=1024/viewers=16/watch=1/frame=64 time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_join_broadcasts/moq\u002dtransport\u002d22/relays=1/publishers=1/broadcasts=1024/viewers=16/watch=1/frame=64 time","culprit":null} -->

- Series: [`rust/relay/relay_session_join_broadcasts/moq-transport-22/relays=1/publishers=1/broadcasts=1024/viewers=16/watch=1/frame=64 time`](https://kixel.quest/factory/series/18815), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-25: 12.94 % against a baseline of 5 %, +7.943 % (+158.9%), at [`420b8dc`](https://github.com/moq-dev/moq/commit/420b8dcb0d9fc1693a90d0cabb6a77e88970c944). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 0.0096.

### `rust/relay/relay_session_join_broadcasts/moq-transport-22/relays=2/publishers=1/broadcasts=1/viewers=16/watch=1/frame=64 time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_join_broadcasts/moq\u002dtransport\u002d22/relays=2/publishers=1/broadcasts=1/viewers=16/watch=1/frame=64 time","culprit":null} -->

- Series: [`rust/relay/relay_session_join_broadcasts/moq-transport-22/relays=2/publishers=1/broadcasts=1/viewers=16/watch=1/frame=64 time`](https://kixel.quest/factory/series/18819), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-25: 13.72 % against a baseline of 5 %, +8.716 % (+174.3%), at [`420b8dc`](https://github.com/moq-dev/moq/commit/420b8dcb0d9fc1693a90d0cabb6a77e88970c944). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 0.0061.

### `rust/relay/relay_session_join_broadcasts/moq-transport-22/relays=2/publishers=1/broadcasts=1024/viewers=16/watch=1/frame=64 time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_join_broadcasts/moq\u002dtransport\u002d22/relays=2/publishers=1/broadcasts=1024/viewers=16/watch=1/frame=64 time","culprit":null} -->

- Series: [`rust/relay/relay_session_join_broadcasts/moq-transport-22/relays=2/publishers=1/broadcasts=1024/viewers=16/watch=1/frame=64 time`](https://kixel.quest/factory/series/18818), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-25: 13.26 % against a baseline of 5 %, +8.256 % (+165.1%), at [`420b8dc`](https://github.com/moq-dev/moq/commit/420b8dcb0d9fc1693a90d0cabb6a77e88970c944). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 0.0080.

### `rust/relay/relay_session_join_broadcasts/moq-transport-22/relays=2/publishers=1/broadcasts=64/viewers=16/watch=1/frame=64 time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_join_broadcasts/moq\u002dtransport\u002d22/relays=2/publishers=1/broadcasts=64/viewers=16/watch=1/frame=64 time","culprit":null} -->

- Series: [`rust/relay/relay_session_join_broadcasts/moq-transport-22/relays=2/publishers=1/broadcasts=64/viewers=16/watch=1/frame=64 time`](https://kixel.quest/factory/series/18820), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-25: 14.32 % against a baseline of 5 %, +9.322 % (+186.4%), at [`420b8dc`](https://github.com/moq-dev/moq/commit/420b8dcb0d9fc1693a90d0cabb6a77e88970c944). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 0.0042.

### `rust/relay/relay_session_join_relays/moq-lite-07-wip/relays=1/publishers=16/broadcasts=1/viewers=16/watch=1/frame=64 time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_join_relays/moq\u002dlite\u002d07\u002dwip/relays=1/publishers=16/broadcasts=1/viewers=16/watch=1/frame=64 time","culprit":null} -->

- Series: [`rust/relay/relay_session_join_relays/moq-lite-07-wip/relays=1/publishers=16/broadcasts=1/viewers=16/watch=1/frame=64 time`](https://kixel.quest/factory/series/18827), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-25: 14.26 % against a baseline of 5 %, +9.257 % (+185.1%), at [`420b8dc`](https://github.com/moq-dev/moq/commit/420b8dcb0d9fc1693a90d0cabb6a77e88970c944). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 0.0044.

### `rust/relay/relay_session_join_stale/moq-lite-06/relays=1/publishers=32/broadcasts=1/viewers=16/watch=1/frame=64/history=64 time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_join_stale/moq\u002dlite\u002d06/relays=1/publishers=32/broadcasts=1/viewers=16/watch=1/frame=64/history=64 time","culprit":null} -->

- Series: [`rust/relay/relay_session_join_stale/moq-lite-06/relays=1/publishers=32/broadcasts=1/viewers=16/watch=1/frame=64/history=64 time`](https://kixel.quest/factory/series/18837), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-25: 14.27 % against a baseline of 5 %, +9.274 % (+185.5%), at [`420b8dc`](https://github.com/moq-dev/moq/commit/420b8dcb0d9fc1693a90d0cabb6a77e88970c944). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 0.0043.

### `rust/relay/relay_session_join_stale/moq-lite-06/relays=1/publishers=8/broadcasts=1/viewers=16/watch=1/frame=64 time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_join_stale/moq\u002dlite\u002d06/relays=1/publishers=8/broadcasts=1/viewers=16/watch=1/frame=64 time","culprit":null} -->

- Series: [`rust/relay/relay_session_join_stale/moq-lite-06/relays=1/publishers=8/broadcasts=1/viewers=16/watch=1/frame=64 time`](https://kixel.quest/factory/series/18839), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-25: 13.21 % against a baseline of 5 %, +8.208 % (+164.2%), at [`420b8dc`](https://github.com/moq-dev/moq/commit/420b8dcb0d9fc1693a90d0cabb6a77e88970c944). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 0.0083.

### `rust/relay/relay_session_join_stale/moq-transport-22/relays=1/publishers=1/broadcasts=1/viewers=16/watch=1/frame=64/history=1024 time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_join_stale/moq\u002dtransport\u002d22/relays=1/publishers=1/broadcasts=1/viewers=16/watch=1/frame=64/history=1024 time","culprit":null} -->

- Series: [`rust/relay/relay_session_join_stale/moq-transport-22/relays=1/publishers=1/broadcasts=1/viewers=16/watch=1/frame=64/history=1024 time`](https://kixel.quest/factory/series/18853), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-25: 13.5 % against a baseline of 5 %, +8.496 % (+169.9%), at [`420b8dc`](https://github.com/moq-dev/moq/commit/420b8dcb0d9fc1693a90d0cabb6a77e88970c944). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 0.0070.

### `rust/relay/relay_session_join_stale/moq-transport-22/relays=1/publishers=8/broadcasts=1/viewers=16/watch=1/frame=64/history=1024 time`: flake

<!-- kixel.quest finding {"series":"rust/relay/relay_session_join_stale/moq\u002dtransport\u002d22/relays=1/publishers=8/broadcasts=1/viewers=16/watch=1/frame=64/history=1024 time","culprit":null} -->

- Series: [`rust/relay/relay_session_join_stale/moq-transport-22/relays=1/publishers=8/broadcasts=1/viewers=16/watch=1/frame=64/history=1024 time`](https://kixel.quest/factory/series/18859), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-25: 13.09 % against a baseline of 5 %, +8.092 % (+161.8%), at [`420b8dc`](https://github.com/moq-dev/moq/commit/420b8dcb0d9fc1693a90d0cabb6a77e88970c944). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 0.0088.

### `rust/shared/annexb-export-queued-groups/1/16 time`: flake

<!-- kixel.quest finding {"series":"rust/shared/annexb\u002dexport\u002dqueued\u002dgroups/1/16 time","culprit":null} -->

- Series: [`rust/shared/annexb-export-queued-groups/1/16 time`](https://kixel.quest/factory/series/18915), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-25: 15.46 % against a baseline of 5 %, +10.46 % (+209.2%), at [`420b8dc`](https://github.com/moq-dev/moq/commit/420b8dcb0d9fc1693a90d0cabb6a77e88970c944). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 0.0020.

### `rust/shared/cmaf-export-frames/1tracks-1frames/256 time`: flake

<!-- kixel.quest finding {"series":"rust/shared/cmaf\u002dexport\u002dframes/1tracks\u002d1frames/256 time","culprit":null} -->

- Series: [`rust/shared/cmaf-export-frames/1tracks-1frames/256 time`](https://kixel.quest/factory/series/18946), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-25: 13.49 % against a baseline of 5 %, +8.488 % (+169.8%), at [`420b8dc`](https://github.com/moq-dev/moq/commit/420b8dcb0d9fc1693a90d0cabb6a77e88970c944). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 0.0070.

### `rust/shared/cmaf-import-bytes/4tracks-16frames/256 time`: flake

<!-- kixel.quest finding {"series":"rust/shared/cmaf\u002dimport\u002dbytes/4tracks\u002d16frames/256 time","culprit":null} -->

- Series: [`rust/shared/cmaf-import-bytes/4tracks-16frames/256 time`](https://kixel.quest/factory/series/18962), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-25: 19.19 % against a baseline of 5 %, +14.19 % (+283.8%), at [`420b8dc`](https://github.com/moq-dev/moq/commit/420b8dcb0d9fc1693a90d0cabb6a77e88970c944). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 0.00012.

### `rust/shared/cmaf-import-frames/4tracks-16frames/256 time`: flake

<!-- kixel.quest finding {"series":"rust/shared/cmaf\u002dimport\u002dframes/4tracks\u002d16frames/256 time","culprit":null} -->

- Series: [`rust/shared/cmaf-import-frames/4tracks-16frames/256 time`](https://kixel.quest/factory/series/18974), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-25: 14.66 % against a baseline of 5 %, +9.656 % (+193.1%), at [`420b8dc`](https://github.com/moq-dev/moq/commit/420b8dcb0d9fc1693a90d0cabb6a77e88970c944). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 0.0034.

### `rust/shared/cmaf-unwrap-bytes/4tracks-16frames/256 time`: flake

<!-- kixel.quest finding {"series":"rust/shared/cmaf\u002dunwrap\u002dbytes/4tracks\u002d16frames/256 time","culprit":null} -->

- Series: [`rust/shared/cmaf-unwrap-bytes/4tracks-16frames/256 time`](https://kixel.quest/factory/series/19041), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-25: 24.58 % against a baseline of 5 %, +19.58 % (+391.6%), at [`420b8dc`](https://github.com/moq-dev/moq/commit/420b8dcb0d9fc1693a90d0cabb6a77e88970c944). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 8.8e-7.

### `rust/shared/cmaf-unwrap-frames/4tracks-16frames/256 time`: flake

<!-- kixel.quest finding {"series":"rust/shared/cmaf\u002dunwrap\u002dframes/4tracks\u002d16frames/256 time","culprit":null} -->

- Series: [`rust/shared/cmaf-unwrap-frames/4tracks-16frames/256 time`](https://kixel.quest/factory/series/19053), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-25: 19.28 % against a baseline of 5 %, +14.28 % (+285.6%), at [`420b8dc`](https://github.com/moq-dev/moq/commit/420b8dcb0d9fc1693a90d0cabb6a77e88970c944). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 0.00012.

### `rust/shared/connection_blocked/live_1024/churn_16 time`: flake

<!-- kixel.quest finding {"series":"rust/shared/connection_blocked/live_1024/churn_16 time","culprit":null} -->

- Series: [`rust/shared/connection_blocked/live_1024/churn_16 time`](https://kixel.quest/factory/series/19335), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-25: 17.49 % against a baseline of 5 %, +12.49 % (+249.9%), at [`420b8dc`](https://github.com/moq-dev/moq/commit/420b8dcb0d9fc1693a90d0cabb6a77e88970c944). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 0.00047.

### `rust/shared/endpoint_route/connections_4096/cids_2 time`: flake

<!-- kixel.quest finding {"series":"rust/shared/endpoint_route/connections_4096/cids_2 time","culprit":null} -->

- Series: [`rust/shared/endpoint_route/connections_4096/cids_2 time`](https://kixel.quest/factory/series/19287), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-25: 26.75 % against a baseline of 5 %, +21.75 % (+435%), at [`420b8dc`](https://github.com/moq-dev/moq/commit/420b8dcb0d9fc1693a90d0cabb6a77e88970c944). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 8.8e-8.

### `rust/shared/endpoint_route/connections_4096/cids_5 time`: flake

<!-- kixel.quest finding {"series":"rust/shared/endpoint_route/connections_4096/cids_5 time","culprit":null} -->

- Series: [`rust/shared/endpoint_route/connections_4096/cids_5 time`](https://kixel.quest/factory/series/19288), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-25: 13.97 % against a baseline of 5 %, +8.967 % (+179.3%), at [`420b8dc`](https://github.com/moq-dev/moq/commit/420b8dcb0d9fc1693a90d0cabb6a77e88970c944). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 0.0052.

### `rust/shared/group_churn/subs_4096/churn_16 time`: flake

<!-- kixel.quest finding {"series":"rust/shared/group_churn/subs_4096/churn_16 time","culprit":null} -->

- Series: [`rust/shared/group_churn/subs_4096/churn_16 time`](https://kixel.quest/factory/series/19341), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-25: 44.99 % against a baseline of 5 %, +39.99 % (+799.7%), at [`420b8dc`](https://github.com/moq-dev/moq/commit/420b8dcb0d9fc1693a90d0cabb6a77e88970c944). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 2.3e-19.

### `rust/shared/group_churn/subs_4096/churn_256 time`: flake

<!-- kixel.quest finding {"series":"rust/shared/group_churn/subs_4096/churn_256 time","culprit":null} -->

- Series: [`rust/shared/group_churn/subs_4096/churn_256 time`](https://kixel.quest/factory/series/19342), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-25: 58.52 % against a baseline of 5 %, +53.52 % (+1070%), at [`420b8dc`](https://github.com/moq-dev/moq/commit/420b8dcb0d9fc1693a90d0cabb6a77e88970c944). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 1.2e-31.

### `rust/shared/receive_evict/streams/4096 time`: flake

<!-- kixel.quest finding {"series":"rust/shared/receive_evict/streams/4096 time","culprit":null} -->

- Series: [`rust/shared/receive_evict/streams/4096 time`](https://kixel.quest/factory/series/19292), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-25: 12.9 % against a baseline of 5 %, +7.903 % (+158.1%), at [`420b8dc`](https://github.com/moq-dev/moq/commit/420b8dcb0d9fc1693a90d0cabb6a77e88970c944). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 0.0099.

### `rust/shared/relay_auth_queue/1/256 time`: flake

<!-- kixel.quest finding {"series":"rust/shared/relay_auth_queue/1/256 time","culprit":null} -->

- Series: [`rust/shared/relay_auth_queue/1/256 time`](https://kixel.quest/factory/series/19438), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-25: 12.96 % against a baseline of 5 %, +7.958 % (+159.2%), at [`420b8dc`](https://github.com/moq-dev/moq/commit/420b8dcb0d9fc1693a90d0cabb6a77e88970c944). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 0.0096.

### `rust/shared/relay_auth_queue/1/4096 time`: flake

<!-- kixel.quest finding {"series":"rust/shared/relay_auth_queue/1/4096 time","culprit":null} -->

- Series: [`rust/shared/relay_auth_queue/1/4096 time`](https://kixel.quest/factory/series/19439), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-25: 16.36 % against a baseline of 5 %, +11.36 % (+227.1%), at [`420b8dc`](https://github.com/moq-dev/moq/commit/420b8dcb0d9fc1693a90d0cabb6a77e88970c944). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 0.0011.

### `rust/shared/schedule/group_priority/queued_4096_changed_16_front time`: flake

<!-- kixel.quest finding {"series":"rust/shared/schedule/group_priority/queued_4096_changed_16_front time","culprit":null} -->

- Series: [`rust/shared/schedule/group_priority/queued_4096_changed_16_front time`](https://kixel.quest/factory/series/19358), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-25: 21.37 % against a baseline of 5 %, +16.37 % (+327.5%), at [`420b8dc`](https://github.com/moq-dev/moq/commit/420b8dcb0d9fc1693a90d0cabb6a77e88970c944). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 0.000019.

### `rust/shared/schedule/group_priority/queued_4096_changed_16_mixed time`: flake

<!-- kixel.quest finding {"series":"rust/shared/schedule/group_priority/queued_4096_changed_16_mixed time","culprit":null} -->

- Series: [`rust/shared/schedule/group_priority/queued_4096_changed_16_mixed time`](https://kixel.quest/factory/series/19359), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-25: 32.19 % against a baseline of 5 %, +27.19 % (+543.9%), at [`420b8dc`](https://github.com/moq-dev/moq/commit/420b8dcb0d9fc1693a90d0cabb6a77e88970c944). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 1.2e-10.

### `rust/shared/schedule/group_retire/queued_256_changed_16_front time`: flake

<!-- kixel.quest finding {"series":"rust/shared/schedule/group_retire/queued_256_changed_16_front time","culprit":null} -->

- Series: [`rust/shared/schedule/group_retire/queued_256_changed_16_front time`](https://kixel.quest/factory/series/19367), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-25: 17.26 % against a baseline of 5 %, +12.26 % (+245.3%), at [`420b8dc`](https://github.com/moq-dev/moq/commit/420b8dcb0d9fc1693a90d0cabb6a77e88970c944). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 0.00056.

### `rust/shared/schedule/group_retire/queued_256_changed_16_mixed time`: flake

<!-- kixel.quest finding {"series":"rust/shared/schedule/group_retire/queued_256_changed_16_mixed time","culprit":null} -->

- Series: [`rust/shared/schedule/group_retire/queued_256_changed_16_mixed time`](https://kixel.quest/factory/series/19368), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-25: 26.04 % against a baseline of 5 %, +21.04 % (+420.7%), at [`420b8dc`](https://github.com/moq-dev/moq/commit/420b8dcb0d9fc1693a90d0cabb6a77e88970c944). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 1.9e-7.

### `rust/shared/schedule/group_retire/queued_256_changed_16_tail time`: flake

<!-- kixel.quest finding {"series":"rust/shared/schedule/group_retire/queued_256_changed_16_tail time","culprit":null} -->

- Series: [`rust/shared/schedule/group_retire/queued_256_changed_16_tail time`](https://kixel.quest/factory/series/19369), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-25: 13.63 % against a baseline of 5 %, +8.625 % (+172.5%), at [`420b8dc`](https://github.com/moq-dev/moq/commit/420b8dcb0d9fc1693a90d0cabb6a77e88970c944). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 0.0064.

### `rust/shared/schedule/group_retire/queued_4096_changed_16_front time`: flake

<!-- kixel.quest finding {"series":"rust/shared/schedule/group_retire/queued_4096_changed_16_front time","culprit":null} -->

- Series: [`rust/shared/schedule/group_retire/queued_4096_changed_16_front time`](https://kixel.quest/factory/series/19370), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-25: 19.05 % against a baseline of 5 %, +14.05 % (+281%), at [`420b8dc`](https://github.com/moq-dev/moq/commit/420b8dcb0d9fc1693a90d0cabb6a77e88970c944). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 0.00014.

### `rust/shared/schedule/group_retire/queued_4096_changed_16_tail time`: flake

<!-- kixel.quest finding {"series":"rust/shared/schedule/group_retire/queued_4096_changed_16_tail time","culprit":null} -->

- Series: [`rust/shared/schedule/group_retire/queued_4096_changed_16_tail time`](https://kixel.quest/factory/series/19372), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-25: 43.39 % against a baseline of 5 %, +38.39 % (+767.9%), at [`420b8dc`](https://github.com/moq-dev/moq/commit/420b8dcb0d9fc1693a90d0cabb6a77e88970c944). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 4.0e-18.

### `rust/shared/schedule/group_retire/queued_4096_changed_256_mixed time`: flake

<!-- kixel.quest finding {"series":"rust/shared/schedule/group_retire/queued_4096_changed_256_mixed time","culprit":null} -->

- Series: [`rust/shared/schedule/group_retire/queued_4096_changed_256_mixed time`](https://kixel.quest/factory/series/19374), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-25: 13.2 % against a baseline of 5 %, +8.198 % (+164%), at [`420b8dc`](https://github.com/moq-dev/moq/commit/420b8dcb0d9fc1693a90d0cabb6a77e88970c944). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 0.0083.

### `rust/shared/send_probe/streams_256/probe_on time`: flake

<!-- kixel.quest finding {"series":"rust/shared/send_probe/streams_256/probe_on time","culprit":null} -->

- Series: [`rust/shared/send_probe/streams_256/probe_on time`](https://kixel.quest/factory/series/19399), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-25: 15.97 % against a baseline of 5 %, +10.97 % (+219.4%), at [`420b8dc`](https://github.com/moq-dev/moq/commit/420b8dcb0d9fc1693a90d0cabb6a77e88970c944). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 0.0014.

### `rust/shared/stats/prefix/10000p_2d_0h time`: flake

<!-- kixel.quest finding {"series":"rust/shared/stats/prefix/10000p_2d_0h time","culprit":null} -->

- Series: [`rust/shared/stats/prefix/10000p_2d_0h time`](https://kixel.quest/factory/series/19555), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-25: 18.13 % against a baseline of 5 %, +13.13 % (+262.6%), at [`420b8dc`](https://github.com/moq-dev/moq/commit/420b8dcb0d9fc1693a90d0cabb6a77e88970c944). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 0.00029.

### `rust/shared/stats/prefix/10000p_2d_100h time`: flake

<!-- kixel.quest finding {"series":"rust/shared/stats/prefix/10000p_2d_100h time","culprit":null} -->

- Series: [`rust/shared/stats/prefix/10000p_2d_100h time`](https://kixel.quest/factory/series/19556), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-25: 16.78 % against a baseline of 5 %, +11.78 % (+235.6%), at [`420b8dc`](https://github.com/moq-dev/moq/commit/420b8dcb0d9fc1693a90d0cabb6a77e88970c944). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 0.00079.

### `rust/shared/stats/prefix/10000p_2d_10h time`: flake

<!-- kixel.quest finding {"series":"rust/shared/stats/prefix/10000p_2d_10h time","culprit":null} -->

- Series: [`rust/shared/stats/prefix/10000p_2d_10h time`](https://kixel.quest/factory/series/19557), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-25: 17.92 % against a baseline of 5 %, +12.92 % (+258.5%), at [`420b8dc`](https://github.com/moq-dev/moq/commit/420b8dcb0d9fc1693a90d0cabb6a77e88970c944). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 0.00034.

### `rust/shared/stats/presence/10000l_0h time`: flake

<!-- kixel.quest finding {"series":"rust/shared/stats/presence/10000l_0h time","culprit":null} -->

- Series: [`rust/shared/stats/presence/10000l_0h time`](https://kixel.quest/factory/series/19567), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-25: 18.59 % against a baseline of 5 %, +13.59 % (+271.9%), at [`420b8dc`](https://github.com/moq-dev/moq/commit/420b8dcb0d9fc1693a90d0cabb6a77e88970c944). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 0.00020.

### `rust/shared/stats/presence/10000l_1000h time`: flake

<!-- kixel.quest finding {"series":"rust/shared/stats/presence/10000l_1000h time","culprit":null} -->

- Series: [`rust/shared/stats/presence/10000l_1000h time`](https://kixel.quest/factory/series/19568), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-25: 21.7 % against a baseline of 5 %, +16.7 % (+334.1%), at [`420b8dc`](https://github.com/moq-dev/moq/commit/420b8dcb0d9fc1693a90d0cabb6a77e88970c944). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 0.000014.

### `rust/shared/stats/presence/10000l_100h time`: flake

<!-- kixel.quest finding {"series":"rust/shared/stats/presence/10000l_100h time","culprit":null} -->

- Series: [`rust/shared/stats/presence/10000l_100h time`](https://kixel.quest/factory/series/19569), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-25: 18.43 % against a baseline of 5 %, +13.43 % (+268.7%), at [`420b8dc`](https://github.com/moq-dev/moq/commit/420b8dcb0d9fc1693a90d0cabb6a77e88970c944). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 0.00023.

### `rust/shared/stats/tick/10000p_1t_0pct time`: flake

<!-- kixel.quest finding {"series":"rust/shared/stats/tick/10000p_1t_0pct time","culprit":null} -->

- Series: [`rust/shared/stats/tick/10000p_1t_0pct time`](https://kixel.quest/factory/series/19578), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-25: 22.62 % against a baseline of 5 %, +17.62 % (+352.4%), at [`420b8dc`](https://github.com/moq-dev/moq/commit/420b8dcb0d9fc1693a90d0cabb6a77e88970c944). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 0.0000061.

### `rust/shared/stats/tick/10000p_1t_100pct time`: flake

<!-- kixel.quest finding {"series":"rust/shared/stats/tick/10000p_1t_100pct time","culprit":null} -->

- Series: [`rust/shared/stats/tick/10000p_1t_100pct time`](https://kixel.quest/factory/series/19579), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-25: 16.77 % against a baseline of 5 %, +11.77 % (+235.3%), at [`420b8dc`](https://github.com/moq-dev/moq/commit/420b8dcb0d9fc1693a90d0cabb6a77e88970c944). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 0.00080.

### `rust/shared/stats/tick/10000p_1t_10pct time`: flake

<!-- kixel.quest finding {"series":"rust/shared/stats/tick/10000p_1t_10pct time","culprit":null} -->

- Series: [`rust/shared/stats/tick/10000p_1t_10pct time`](https://kixel.quest/factory/series/19580), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-25: 18.01 % against a baseline of 5 %, +13.01 % (+260.3%), at [`420b8dc`](https://github.com/moq-dev/moq/commit/420b8dcb0d9fc1693a90d0cabb6a77e88970c944). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 0.00031.

### `rust/shared/stats/tick/10000p_4t_0pct time`: flake

<!-- kixel.quest finding {"series":"rust/shared/stats/tick/10000p_4t_0pct time","culprit":null} -->

- Series: [`rust/shared/stats/tick/10000p_4t_0pct time`](https://kixel.quest/factory/series/19581), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-25: 20.45 % against a baseline of 5 %, +15.45 % (+309%), at [`420b8dc`](https://github.com/moq-dev/moq/commit/420b8dcb0d9fc1693a90d0cabb6a77e88970c944). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 0.000043.

### `rust/shared/stats/tick/50000p_1t_0pct time`: flake

<!-- kixel.quest finding {"series":"rust/shared/stats/tick/50000p_1t_0pct time","culprit":null} -->

- Series: [`rust/shared/stats/tick/50000p_1t_0pct time`](https://kixel.quest/factory/series/19596), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-25: 23.74 % against a baseline of 5 %, +18.74 % (+374.7%), at [`420b8dc`](https://github.com/moq-dev/moq/commit/420b8dcb0d9fc1693a90d0cabb6a77e88970c944). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 0.0000021.

### `rust/shared/ts_si_pending_pcr/4096/1 time`: flake

<!-- kixel.quest finding {"series":"rust/shared/ts_si_pending_pcr/4096/1 time","culprit":null} -->

- Series: [`rust/shared/ts_si_pending_pcr/4096/1 time`](https://kixel.quest/factory/series/19075), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-25: 14.71 % against a baseline of 5 %, +9.709 % (+194.2%), at [`420b8dc`](https://github.com/moq-dev/moq/commit/420b8dcb0d9fc1693a90d0cabb6a77e88970c944). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 0.0033.

### `rust/shared/video-reorder/backward/1024x256 time`: flake

<!-- kixel.quest finding {"series":"rust/shared/video\u002dreorder/backward/1024x256 time","culprit":null} -->

- Series: [`rust/shared/video-reorder/backward/1024x256 time`](https://kixel.quest/factory/series/19136), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-25: 13.84 % against a baseline of 5 %, +8.843 % (+176.9%), at [`420b8dc`](https://github.com/moq-dev/moq/commit/420b8dcb0d9fc1693a90d0cabb6a77e88970c944). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 0.0056.

### `rust/shared/worker_request_reply/connections-16/bytes-262144 time`: flake

<!-- kixel.quest finding {"series":"rust/shared/worker_request_reply/connections\u002d16/bytes\u002d262144 time","culprit":null} -->

- Series: [`rust/shared/worker_request_reply/connections-16/bytes-262144 time`](https://kixel.quest/factory/series/19601), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-25: 50.48 % against a baseline of 5 %, +45.48 % (+909.6%), at [`420b8dc`](https://github.com/moq-dev/moq/commit/420b8dcb0d9fc1693a90d0cabb6a77e88970c944). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 5.7e-24.

### `rust/shared/worker_turns/sockets-1/1 time`: flake

<!-- kixel.quest finding {"series":"rust/shared/worker_turns/sockets\u002d1/1 time","culprit":null} -->

- Series: [`rust/shared/worker_turns/sockets-1/1 time`](https://kixel.quest/factory/series/19605), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-25: 88.21 % against a baseline of 5 %, +83.21 % (+1664%), at [`420b8dc`](https://github.com/moq-dev/moq/commit/420b8dcb0d9fc1693a90d0cabb6a77e88970c944). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 1.2e-69.

### `rust/shared/worker_turns/sockets-1/8 time`: flake

<!-- kixel.quest finding {"series":"rust/shared/worker_turns/sockets\u002d1/8 time","culprit":null} -->

- Series: [`rust/shared/worker_turns/sockets-1/8 time`](https://kixel.quest/factory/series/19607), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-25: 25.56 % against a baseline of 5 %, +20.56 % (+411.2%), at [`420b8dc`](https://github.com/moq-dev/moq/commit/420b8dcb0d9fc1693a90d0cabb6a77e88970c944). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 3.2e-7.

### `rust/shared/worker_turns/sockets-4/1 time`: flake

<!-- kixel.quest finding {"series":"rust/shared/worker_turns/sockets\u002d4/1 time","culprit":null} -->

- Series: [`rust/shared/worker_turns/sockets-4/1 time`](https://kixel.quest/factory/series/19611), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-25: 43.57 % against a baseline of 5 %, +38.57 % (+771.3%), at [`420b8dc`](https://github.com/moq-dev/moq/commit/420b8dcb0d9fc1693a90d0cabb6a77e88970c944). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 3.0e-18.

### `rust/uring-echo_quic/echo_quic/udp/all-off time`: flake

<!-- kixel.quest finding {"series":"rust/uring\u002decho_quic/echo_quic/udp/all\u002doff time","culprit":null} -->

- Series: [`rust/uring-echo_quic/echo_quic/udp/all-off time`](https://kixel.quest/factory/series/19755), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-25: 60.45 % against a baseline of 5 %, +55.45 % (+1109%), at [`420b8dc`](https://github.com/moq-dev/moq/commit/420b8dcb0d9fc1693a90d0cabb6a77e88970c944). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 1.2e-33.

### `rust/uring-echo_quic/echo_quic/udp/all-on time`: flake

<!-- kixel.quest finding {"series":"rust/uring\u002decho_quic/echo_quic/udp/all\u002don time","culprit":null} -->

- Series: [`rust/uring-echo_quic/echo_quic/udp/all-on time`](https://kixel.quest/factory/series/19756), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-25: 14.68 % against a baseline of 5 %, +9.684 % (+193.7%), at [`420b8dc`](https://github.com/moq-dev/moq/commit/420b8dcb0d9fc1693a90d0cabb6a77e88970c944). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 0.0033.

### `rust/uring-echo_quic/echo_quic/udp/oneshot time`: flake

<!-- kixel.quest finding {"series":"rust/uring\u002decho_quic/echo_quic/udp/oneshot time","culprit":null} -->

- Series: [`rust/uring-echo_quic/echo_quic/udp/oneshot time`](https://kixel.quest/factory/series/19759), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-25: 14.01 % against a baseline of 5 %, +9.012 % (+180.2%), at [`420b8dc`](https://github.com/moq-dev/moq/commit/420b8dcb0d9fc1693a90d0cabb6a77e88970c944). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 0.0051.

### `rust/uring-web_headers/web_headers/pending-0/ready-1 time`: flake

<!-- kixel.quest finding {"series":"rust/uring\u002dweb_headers/web_headers/pending\u002d0/ready\u002d1 time","culprit":null} -->

- Series: [`rust/uring-web_headers/web_headers/pending-0/ready-1 time`](https://kixel.quest/factory/series/19778), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-25: 19.09 % against a baseline of 5 %, +14.09 % (+281.7%), at [`420b8dc`](https://github.com/moq-dev/moq/commit/420b8dcb0d9fc1693a90d0cabb6a77e88970c944). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 0.00013.
