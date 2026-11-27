# [M] Investigate rust/auth/verify/decide/HS256/set time, rust/auth/verify/decide/HS384/dir time, and 40 more

## Goal

Kixel Quest measured what the Plan lists and filed it here on its own. For each finding:

1. Diagnose its cause, starting from the culprit's diff and sessions where it has them.
2. Have a second session, one that did not write the diagnosis, try to refute it, and diagnose again when it does.
3. Fix it, accept it with the reason recorded in this Plan, or improve on it further. A fix carries the measurement that shows it.

Then look past the finding: make the related improvements the diagnosis turns up where they fit here, and add the rest as follow-up quests.

## Plan

Each finding keeps the comment Kixel Quest finds it by, so a later run that sees it again adds its sighting here instead of filing it anew.

### `rust/auth/verify/decide/HS256/set time`: flake

<!-- kixel.quest finding {"series":"rust/auth/verify/decide/HS256/set time","culprit":null} -->

- Series: [`rust/auth/verify/decide/HS256/set time`](https://kixel.quest/factory/series/18343?repository=508763694), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-27: 16.31 % against a baseline of 5 %, +11.31 % (+226.2%), at [`f7a41c3`](https://github.com/moq-dev/moq/commit/f7a41c3ea52fa6b12c6bf698142f428c24ea673b). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 0.0011.

### `rust/auth/verify/decide/HS384/dir time`: flake

<!-- kixel.quest finding {"series":"rust/auth/verify/decide/HS384/dir time","culprit":null} -->

- Series: [`rust/auth/verify/decide/HS384/dir time`](https://kixel.quest/factory/series/18344?repository=508763694), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-27: 15.58 % against a baseline of 5 %, +10.58 % (+211.6%), at [`f7a41c3`](https://github.com/moq-dev/moq/commit/f7a41c3ea52fa6b12c6bf698142f428c24ea673b). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 0.0018.

### `rust/auth/verify/decide/HS512/set time`: flake

<!-- kixel.quest finding {"series":"rust/auth/verify/decide/HS512/set time","culprit":null} -->

- Series: [`rust/auth/verify/decide/HS512/set time`](https://kixel.quest/factory/series/18349?repository=508763694), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-27: 14.46 % against a baseline of 5 %, +9.46 % (+189.2%), at [`f7a41c3`](https://github.com/moq-dev/moq/commit/f7a41c3ea52fa6b12c6bf698142f428c24ea673b). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 0.0038.

### `rust/auth/verify/set/1/missing time`: flake

<!-- kixel.quest finding {"series":"rust/auth/verify/set/1/missing time","culprit":null} -->

- Series: [`rust/auth/verify/set/1/missing time`](https://kixel.quest/factory/series/18383?repository=508763694), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-27: 14.37 % against a baseline of 5 %, +9.367 % (+187.3%), at [`f7a41c3`](https://github.com/moq-dev/moq/commit/f7a41c3ea52fa6b12c6bf698142f428c24ea673b). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 0.0041.

### `rust/auth/verify/set/16/missing time`: flake

<!-- kixel.quest finding {"series":"rust/auth/verify/set/16/missing time","culprit":null} -->

- Series: [`rust/auth/verify/set/16/missing time`](https://kixel.quest/factory/series/18380?repository=508763694), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-27: 13.18 % against a baseline of 5 %, +8.176 % (+163.5%), at [`f7a41c3`](https://github.com/moq-dev/moq/commit/f7a41c3ea52fa6b12c6bf698142f428c24ea673b). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 0.0084.

### `rust/extra/decode_clock_drain/flat/4096B/10000 time`: flake

<!-- kixel.quest finding {"series":"rust/extra/decode_clock_drain/flat/4096B/10000 time","culprit":null} -->

- Series: [`rust/extra/decode_clock_drain/flat/4096B/10000 time`](https://kixel.quest/factory/series/18102?repository=508763694), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-27: 17.36 % against a baseline of 5 %, +12.36 % (+247.2%), at [`f7a41c3`](https://github.com/moq-dev/moq/commit/f7a41c3ea52fa6b12c6bf698142f428c24ea673b). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 0.00052.

### `rust/extra/decode_clock_drain/flat/64B/10000 time`: flake

<!-- kixel.quest finding {"series":"rust/extra/decode_clock_drain/flat/64B/10000 time","culprit":null} -->

- Series: [`rust/extra/decode_clock_drain/flat/64B/10000 time`](https://kixel.quest/factory/series/18105?repository=508763694), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-27: 19.25 % against a baseline of 5 %, +14.25 % (+284.9%), at [`f7a41c3`](https://github.com/moq-dev/moq/commit/f7a41c3ea52fa6b12c6bf698142f428c24ea673b). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 0.00012.

### `rust/extra/fanout/wall/encoder/1 time`: flake

<!-- kixel.quest finding {"series":"rust/extra/fanout/wall/encoder/1 time","culprit":null} -->

- Series: [`rust/extra/fanout/wall/encoder/1 time`](https://kixel.quest/factory/series/18280?repository=508763694), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-27: 27.77 % against a baseline of 5 %, +22.77 % (+455.4%), at [`f7a41c3`](https://github.com/moq-dev/moq/commit/f7a41c3ea52fa6b12c6bf698142f428c24ea673b). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 2.8e-8.

### `rust/extra/fanout/worker/encoder/1 time`: flake

<!-- kixel.quest finding {"series":"rust/extra/fanout/worker/encoder/1 time","culprit":null} -->

- Series: [`rust/extra/fanout/worker/encoder/1 time`](https://kixel.quest/factory/series/18284?repository=508763694), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-27: 30.63 % against a baseline of 5 %, +25.63 % (+512.7%), at [`f7a41c3`](https://github.com/moq-dev/moq/commit/f7a41c3ea52fa6b12c6bf698142f428c24ea673b). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 9.0e-10.

### `rust/extra/fanout/worker/encoder/3 time`: flake

<!-- kixel.quest finding {"series":"rust/extra/fanout/worker/encoder/3 time","culprit":null} -->

- Series: [`rust/extra/fanout/worker/encoder/3 time`](https://kixel.quest/factory/series/18285?repository=508763694), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-27: 30 % against a baseline of 5 %, +25 % (+500%), at [`f7a41c3`](https://github.com/moq-dev/moq/commit/f7a41c3ea52fa6b12c6bf698142f428c24ea673b). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 2.0e-9.

### `rust/extra/h264-split-pps/distinct/4096 time`: flake

<!-- kixel.quest finding {"series":"rust/extra/h264\u002dsplit\u002dpps/distinct/4096 time","culprit":null} -->

- Series: [`rust/extra/h264-split-pps/distinct/4096 time`](https://kixel.quest/factory/series/18173?repository=508763694), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-27: 25.73 % against a baseline of 5 %, +20.73 % (+414.7%), at [`f7a41c3`](https://github.com/moq-dev/moq/commit/f7a41c3ea52fa6b12c6bf698142f428c24ea673b). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 2.7e-7.

### `rust/extra/h264-split-pps/repeated/4096 time`: flake

<!-- kixel.quest finding {"series":"rust/extra/h264\u002dsplit\u002dpps/repeated/4096 time","culprit":null} -->

- Series: [`rust/extra/h264-split-pps/repeated/4096 time`](https://kixel.quest/factory/series/18177?repository=508763694), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-27: 32.72 % against a baseline of 5 %, +27.72 % (+554.5%), at [`f7a41c3`](https://github.com/moq-dev/moq/commit/f7a41c3ea52fa6b12c6bf698142f428c24ea673b). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 6.0e-11.

### `rust/extra/jitter_acquisition/16/256 time`: flake

<!-- kixel.quest finding {"series":"rust/extra/jitter_acquisition/16/256 time","culprit":null} -->

- Series: [`rust/extra/jitter_acquisition/16/256 time`](https://kixel.quest/factory/series/18126?repository=508763694), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-27: 26.29 % against a baseline of 5 %, +21.29 % (+425.8%), at [`f7a41c3`](https://github.com/moq-dev/moq/commit/f7a41c3ea52fa6b12c6bf698142f428c24ea673b). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 1.5e-7.

### `rust/extra/jitter_acquisition/64/64 time`: flake

<!-- kixel.quest finding {"series":"rust/extra/jitter_acquisition/64/64 time","culprit":null} -->

- Series: [`rust/extra/jitter_acquisition/64/64 time`](https://kixel.quest/factory/series/18133?repository=508763694), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-27: 26.8 % against a baseline of 5 %, +21.8 % (+436%), at [`f7a41c3`](https://github.com/moq-dev/moq/commit/f7a41c3ea52fa6b12c6bf698142f428c24ea673b). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 8.3e-8.

### `rust/extra/jitter_admission/16/1024 time`: flake

<!-- kixel.quest finding {"series":"rust/extra/jitter_admission/16/1024 time","culprit":null} -->

- Series: [`rust/extra/jitter_admission/16/1024 time`](https://kixel.quest/factory/series/18137?repository=508763694), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-27: 26.7 % against a baseline of 5 %, +21.7 % (+434%), at [`f7a41c3`](https://github.com/moq-dev/moq/commit/f7a41c3ea52fa6b12c6bf698142f428c24ea673b). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 9.3e-8.

### `rust/extra/jitter_admission/4/256 time`: flake

<!-- kixel.quest finding {"series":"rust/extra/jitter_admission/4/256 time","culprit":null} -->

- Series: [`rust/extra/jitter_admission/4/256 time`](https://kixel.quest/factory/series/18141?repository=508763694), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-27: 29.49 % against a baseline of 5 %, +24.49 % (+489.8%), at [`f7a41c3`](https://github.com/moq-dev/moq/commit/f7a41c3ea52fa6b12c6bf698142f428c24ea673b). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 3.7e-9.

### `rust/extra/jitter_admission/4/64 time`: flake

<!-- kixel.quest finding {"series":"rust/extra/jitter_admission/4/64 time","culprit":null} -->

- Series: [`rust/extra/jitter_admission/4/64 time`](https://kixel.quest/factory/series/18142?repository=508763694), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-27: 28.7 % against a baseline of 5 %, +23.7 % (+474%), at [`f7a41c3`](https://github.com/moq-dev/moq/commit/f7a41c3ea52fa6b12c6bf698142f428c24ea673b). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 9.5e-9.

### `rust/extra/jitter_handoff/16/256 time`: flake

<!-- kixel.quest finding {"series":"rust/extra/jitter_handoff/16/256 time","culprit":null} -->

- Series: [`rust/extra/jitter_handoff/16/256 time`](https://kixel.quest/factory/series/18150?repository=508763694), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-27: 19.14 % against a baseline of 5 %, +14.14 % (+282.8%), at [`f7a41c3`](https://github.com/moq-dev/moq/commit/f7a41c3ea52fa6b12c6bf698142f428c24ea673b). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 0.00013.

### `rust/extra/native_transport/bytes-64/streams-1 time`: flake

<!-- kixel.quest finding {"series":"rust/extra/native_transport/bytes\u002d64/streams\u002d1 time","culprit":null} -->

- Series: [`rust/extra/native_transport/bytes-64/streams-1 time`](https://kixel.quest/factory/series/18272?repository=508763694), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-27: 12.88 % against a baseline of 5 %, +7.882 % (+157.6%), at [`f7a41c3`](https://github.com/moq-dev/moq/commit/f7a41c3ea52fa6b12c6bf698142f428c24ea673b). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 0.010.

### `rust/extra/table/forced-snapshots/100%/1000 time`: flake

<!-- kixel.quest finding {"series":"rust/extra/table/forced\u002dsnapshots/100%/1000 time","culprit":null} -->

- Series: [`rust/extra/table/forced-snapshots/100%/1000 time`](https://kixel.quest/factory/series/18080?repository=508763694), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-27: 13.21 % against a baseline of 5 %, +8.208 % (+164.2%), at [`f7a41c3`](https://github.com/moq-dev/moq/commit/f7a41c3ea52fa6b12c6bf698142f428c24ea673b). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 0.0083.

### `rust/msf/msf_serialize/256tracks/16entries time`: flake

<!-- kixel.quest finding {"series":"rust/msf/msf_serialize/256tracks/16entries time","culprit":null} -->

- Series: [`rust/msf/msf_serialize/256tracks/16entries time`](https://kixel.quest/factory/series/17936?repository=508763694), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-27: 16.96 % against a baseline of 5 %, +11.96 % (+239.2%), at [`f7a41c3`](https://github.com/moq-dev/moq/commit/f7a41c3ea52fa6b12c6bf698142f428c24ea673b). Spread as two pooled standard deviations from each commit's mean, over 2 observations of 1 commit, against the 5% regression it must detect; chi-square test p = 0.00069.

### `rust/shared/annexb-export-frames/1tracks-1frames/256 time`: flake

<!-- kixel.quest finding {"series":"rust/shared/annexb\u002dexport\u002dframes/1tracks\u002d1frames/256 time","culprit":null} -->

- Series: [`rust/shared/annexb-export-frames/1tracks-1frames/256 time`](https://kixel.quest/factory/series/18910?repository=508763694), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-27: 10.87 % against a baseline of 5 %, +5.874 % (+117.5%), at [`f7a41c3`](https://github.com/moq-dev/moq/commit/f7a41c3ea52fa6b12c6bf698142f428c24ea673b). Spread as two pooled standard deviations from each commit's mean, over 6 observations of 3 commits, against the 5% regression it must detect; chi-square test p = 0.0027.

### `rust/shared/cmaf-export-bytes/1tracks-1frames/4096 time`: flake

<!-- kixel.quest finding {"series":"rust/shared/cmaf\u002dexport\u002dbytes/1tracks\u002d1frames/4096 time","culprit":null} -->

- Series: [`rust/shared/cmaf-export-bytes/1tracks-1frames/4096 time`](https://kixel.quest/factory/series/18935?repository=508763694), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-27: 10.43 % against a baseline of 5 %, +5.429 % (+108.6%), at [`f7a41c3`](https://github.com/moq-dev/moq/commit/f7a41c3ea52fa6b12c6bf698142f428c24ea673b). Spread as two pooled standard deviations from each commit's mean, over 6 observations of 3 commits, against the 5% regression it must detect; chi-square test p = 0.0045.

### `rust/shared/cmaf-interleaved-import/4tracks/16runs-4096bytes-1024chunk time`: flake

<!-- kixel.quest finding {"series":"rust/shared/cmaf\u002dinterleaved\u002dimport/4tracks/16runs\u002d4096bytes\u002d1024chunk time","culprit":null} -->

- Series: [`rust/shared/cmaf-interleaved-import/4tracks/16runs-4096bytes-1024chunk time`](https://kixel.quest/factory/series/18995?repository=508763694), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-27: 24.09 % against a baseline of 5 %, +19.09 % (+381.8%), at [`f7a41c3`](https://github.com/moq-dev/moq/commit/f7a41c3ea52fa6b12c6bf698142f428c24ea673b). Spread as two pooled standard deviations from each commit's mean, over 6 observations of 3 commits, against the 5% regression it must detect; chi-square test p = 5.1e-15.

### `rust/shared/container-export/mkv/1x1 time`: flake

<!-- kixel.quest finding {"series":"rust/shared/container\u002dexport/mkv/1x1 time","culprit":null} -->

- Series: [`rust/shared/container-export/mkv/1x1 time`](https://kixel.quest/factory/series/19111?repository=508763694), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-27: 9.975 % against a baseline of 5 %, +4.975 % (+99.49%), at [`f7a41c3`](https://github.com/moq-dev/moq/commit/f7a41c3ea52fa6b12c6bf698142f428c24ea673b). Spread as two pooled standard deviations from each commit's mean, over 6 observations of 3 commits, against the 5% regression it must detect; chi-square test p = 0.0076.

### `rust/shared/container-export/ts/1x1 time`: flake

<!-- kixel.quest finding {"series":"rust/shared/container\u002dexport/ts/1x1 time","culprit":null} -->

- Series: [`rust/shared/container-export/ts/1x1 time`](https://kixel.quest/factory/series/19120?repository=508763694), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-27: 15.02 % against a baseline of 5 %, +10.02 % (+200.4%), at [`f7a41c3`](https://github.com/moq-dev/moq/commit/f7a41c3ea52fa6b12c6bf698142f428c24ea673b). Spread as two pooled standard deviations from each commit's mean, over 6 observations of 3 commits, against the 5% regression it must detect; chi-square test p = 0.0000057.

### `rust/shared/receive_evict/streams/4096 time`: flake

<!-- kixel.quest finding {"series":"rust/shared/receive_evict/streams/4096 time","culprit":null} -->

- Series: [`rust/shared/receive_evict/streams/4096 time`](https://kixel.quest/factory/series/19292?repository=508763694), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-27: 9.839 % against a baseline of 5 %, +4.839 % (+96.77%), at [`f7a41c3`](https://github.com/moq-dev/moq/commit/f7a41c3ea52fa6b12c6bf698142f428c24ea673b). Spread as two pooled standard deviations from each commit's mean, over 6 observations of 3 commits, against the 5% regression it must detect; chi-square test p = 0.0088.

### `rust/shared/relay_auth_queue/128/4096 time`: flake

<!-- kixel.quest finding {"series":"rust/shared/relay_auth_queue/128/4096 time","culprit":null} -->

- Series: [`rust/shared/relay_auth_queue/128/4096 time`](https://kixel.quest/factory/series/19442?repository=508763694), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-27: 20.32 % against a baseline of 5 %, +15.32 % (+306.3%), at [`f7a41c3`](https://github.com/moq-dev/moq/commit/f7a41c3ea52fa6b12c6bf698142f428c24ea673b). Spread as two pooled standard deviations from each commit's mean, over 6 observations of 3 commits, against the 5% regression it must detect; chi-square test p = 1.0e-10.

### `rust/shared/relay_http_admission/connections=1/64 time`: flake

<!-- kixel.quest finding {"series":"rust/shared/relay_http_admission/connections=1/64 time","culprit":null} -->

- Series: [`rust/shared/relay_http_admission/connections=1/64 time`](https://kixel.quest/factory/series/19457?repository=508763694), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-27: 10.08 % against a baseline of 5 %, +5.079 % (+101.6%), at [`f7a41c3`](https://github.com/moq-dev/moq/commit/f7a41c3ea52fa6b12c6bf698142f428c24ea673b). Spread as two pooled standard deviations from each commit's mean, over 6 observations of 3 commits, against the 5% regression it must detect; chi-square test p = 0.0068.

### `rust/shared/schedule/group_priority/queued_4096_changed_256_mixed time`: flake

<!-- kixel.quest finding {"series":"rust/shared/schedule/group_priority/queued_4096_changed_256_mixed time","culprit":null} -->

- Series: [`rust/shared/schedule/group_priority/queued_4096_changed_256_mixed time`](https://kixel.quest/factory/series/19362?repository=508763694), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-27: 13.38 % against a baseline of 5 %, +8.377 % (+167.5%), at [`f7a41c3`](https://github.com/moq-dev/moq/commit/f7a41c3ea52fa6b12c6bf698142f428c24ea673b). Spread as two pooled standard deviations from each commit's mean, over 6 observations of 3 commits, against the 5% regression it must detect; chi-square test p = 0.000084.

### `rust/shared/schedule/group_retire/queued_256_changed_16_tail time`: flake

<!-- kixel.quest finding {"series":"rust/shared/schedule/group_retire/queued_256_changed_16_tail time","culprit":null} -->

- Series: [`rust/shared/schedule/group_retire/queued_256_changed_16_tail time`](https://kixel.quest/factory/series/19369?repository=508763694), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-27: 9.969 % against a baseline of 5 %, +4.969 % (+99.37%), at [`f7a41c3`](https://github.com/moq-dev/moq/commit/f7a41c3ea52fa6b12c6bf698142f428c24ea673b). Spread as two pooled standard deviations from each commit's mean, over 6 observations of 3 commits, against the 5% regression it must detect; chi-square test p = 0.0076.

### `rust/shared/schedule/group_retire/queued_4096_changed_16_mixed time`: flake

<!-- kixel.quest finding {"series":"rust/shared/schedule/group_retire/queued_4096_changed_16_mixed time","culprit":null} -->

- Series: [`rust/shared/schedule/group_retire/queued_4096_changed_16_mixed time`](https://kixel.quest/factory/series/19371?repository=508763694), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-27: 16.54 % against a baseline of 5 %, +11.54 % (+230.7%), at [`f7a41c3`](https://github.com/moq-dev/moq/commit/f7a41c3ea52fa6b12c6bf698142f428c24ea673b). Spread as two pooled standard deviations from each commit's mean, over 6 observations of 3 commits, against the 5% regression it must detect; chi-square test p = 3.5e-7.

### `rust/shared/stats/prefix/10000p_8d_0h time`: flake

<!-- kixel.quest finding {"series":"rust/shared/stats/prefix/10000p_8d_0h time","culprit":null} -->

- Series: [`rust/shared/stats/prefix/10000p_8d_0h time`](https://kixel.quest/factory/series/19558?repository=508763694), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-27: 10.29 % against a baseline of 5 %, +5.291 % (+105.8%), at [`f7a41c3`](https://github.com/moq-dev/moq/commit/f7a41c3ea52fa6b12c6bf698142f428c24ea673b). Spread as two pooled standard deviations from each commit's mean, over 6 observations of 3 commits, against the 5% regression it must detect; chi-square test p = 0.0053.

### `rust/shared/stats/prefix/10000p_8d_100h time`: flake

<!-- kixel.quest finding {"series":"rust/shared/stats/prefix/10000p_8d_100h time","culprit":null} -->

- Series: [`rust/shared/stats/prefix/10000p_8d_100h time`](https://kixel.quest/factory/series/19559?repository=508763694), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-27: 11.03 % against a baseline of 5 %, +6.032 % (+120.6%), at [`f7a41c3`](https://github.com/moq-dev/moq/commit/f7a41c3ea52fa6b12c6bf698142f428c24ea673b). Spread as two pooled standard deviations from each commit's mean, over 6 observations of 3 commits, against the 5% regression it must detect; chi-square test p = 0.0022.

### `rust/shared/stats/prefix/10000p_8d_10h time`: flake

<!-- kixel.quest finding {"series":"rust/shared/stats/prefix/10000p_8d_10h time","culprit":null} -->

- Series: [`rust/shared/stats/prefix/10000p_8d_10h time`](https://kixel.quest/factory/series/19560?repository=508763694), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-27: 11.78 % against a baseline of 5 %, +6.777 % (+135.5%), at [`f7a41c3`](https://github.com/moq-dev/moq/commit/f7a41c3ea52fa6b12c6bf698142f428c24ea673b). Spread as two pooled standard deviations from each commit's mean, over 6 observations of 3 commits, against the 5% regression it must detect; chi-square test p = 0.00084.

### `rust/shared/stats/roots/1000p_1000r time`: flake

<!-- kixel.quest finding {"series":"rust/shared/stats/roots/1000p_1000r time","culprit":null} -->

- Series: [`rust/shared/stats/roots/1000p_1000r time`](https://kixel.quest/factory/series/19575?repository=508763694), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-27: 11.33 % against a baseline of 5 %, +6.331 % (+126.6%), at [`f7a41c3`](https://github.com/moq-dev/moq/commit/f7a41c3ea52fa6b12c6bf698142f428c24ea673b). Spread as two pooled standard deviations from each commit's mean, over 6 observations of 3 commits, against the 5% regression it must detect; chi-square test p = 0.0015.

### `rust/shared/ts_si_renewal_pcr/4096/16 time`: flake

<!-- kixel.quest finding {"series":"rust/shared/ts_si_renewal_pcr/4096/16 time","culprit":null} -->

- Series: [`rust/shared/ts_si_renewal_pcr/4096/16 time`](https://kixel.quest/factory/series/19085?repository=508763694), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-27: 15.21 % against a baseline of 5 %, +10.21 % (+204.3%), at [`f7a41c3`](https://github.com/moq-dev/moq/commit/f7a41c3ea52fa6b12c6bf698142f428c24ea673b). Spread as two pooled standard deviations from each commit's mean, over 6 observations of 3 commits, against the 5% regression it must detect; chi-square test p = 0.0000041.

### `rust/shared/video-reorder/backward/256x16384 time`: flake

<!-- kixel.quest finding {"series":"rust/shared/video\u002dreorder/backward/256x16384 time","culprit":null} -->

- Series: [`rust/shared/video-reorder/backward/256x16384 time`](https://kixel.quest/factory/series/19137?repository=508763694), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-27: 11.38 % against a baseline of 5 %, +6.381 % (+127.6%), at [`f7a41c3`](https://github.com/moq-dev/moq/commit/f7a41c3ea52fa6b12c6bf698142f428c24ea673b). Spread as two pooled standard deviations from each commit's mean, over 6 observations of 3 commits, against the 5% regression it must detect; chi-square test p = 0.0014.

### `rust/uring-echo_quic/echo_quic/udp/no-gro time`: flake

<!-- kixel.quest finding {"series":"rust/uring\u002decho_quic/echo_quic/udp/no\u002dgro time","culprit":null} -->

- Series: [`rust/uring-echo_quic/echo_quic/udp/no-gro time`](https://kixel.quest/factory/series/19757?repository=508763694), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-27: 13.52 % against a baseline of 5 %, +8.521 % (+170.4%), at [`f7a41c3`](https://github.com/moq-dev/moq/commit/f7a41c3ea52fa6b12c6bf698142f428c24ea673b). Spread as two pooled standard deviations from each commit's mean, over 6 observations of 3 commits, against the 5% regression it must detect; chi-square test p = 0.000067.

### `rust/uring-timers/timers/rearm/1 time`: flake

<!-- kixel.quest finding {"series":"rust/uring\u002dtimers/timers/rearm/1 time","culprit":null} -->

- Series: [`rust/uring-timers/timers/rearm/1 time`](https://kixel.quest/factory/series/19774?repository=508763694), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-27: 14.97 % against a baseline of 5 %, +9.967 % (+199.3%), at [`f7a41c3`](https://github.com/moq-dev/moq/commit/f7a41c3ea52fa6b12c6bf698142f428c24ea673b). Spread as two pooled standard deviations from each commit's mean, over 6 observations of 3 commits, against the 5% regression it must detect; chi-square test p = 0.0000062.

### `rust/uring-udp_tokio/udp_tokio_epoll/recvmmsg_gro-off_gso-on time`: flake

<!-- kixel.quest finding {"series":"rust/uring\u002dudp_tokio/udp_tokio_epoll/recvmmsg_gro\u002doff_gso\u002don time","culprit":null} -->

- Series: [`rust/uring-udp_tokio/udp_tokio_epoll/recvmmsg_gro-off_gso-on time`](https://kixel.quest/factory/series/19744?repository=508763694), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-27: 14.08 % against a baseline of 5 %, +9.076 % (+181.5%), at [`f7a41c3`](https://github.com/moq-dev/moq/commit/f7a41c3ea52fa6b12c6bf698142f428c24ea673b). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 0.00036.

### `rust/uring-udp_tokio/udp_tokio_epoll/recvmsg_gro-off_gso-on time`: flake

<!-- kixel.quest finding {"series":"rust/uring\u002dudp_tokio/udp_tokio_epoll/recvmsg_gro\u002doff_gso\u002don time","culprit":null} -->

- Series: [`rust/uring-udp_tokio/udp_tokio_epoll/recvmsg_gro-off_gso-on time`](https://kixel.quest/factory/series/19748?repository=508763694), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-27: 11.29 % against a baseline of 5 %, +6.29 % (+125.8%), at [`f7a41c3`](https://github.com/moq-dev/moq/commit/f7a41c3ea52fa6b12c6bf698142f428c24ea673b). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 0.0061.
