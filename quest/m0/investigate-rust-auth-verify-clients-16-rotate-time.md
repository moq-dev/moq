# [M] Investigate rust/auth/verify/clients/16/rotate time, rust/auth/verify/decide/HS256/dir time, and 64 more

## Goal

Kixel Quest measured what the Plan lists and filed it here on its own. For each finding:

1. Diagnose its cause, starting from the culprit's diff and sessions where it has them.
2. Have a second session, one that did not write the diagnosis, try to refute it, and diagnose again when it does.
3. Fix it, accept it with the reason recorded in this Plan, or improve on it further. A fix carries the measurement that shows it.

Then look past the finding: make the related improvements the diagnosis turns up where they fit here, and add the rest as follow-up quests.

## Plan

Each finding keeps the comment Kixel Quest finds it by, so a later run that sees it again adds its sighting here instead of filing it anew.

### `rust/auth/verify/clients/16/rotate time`: flake

<!-- kixel.quest finding {"series":"rust/auth/verify/clients/16/rotate time","culprit":null} -->

- Series: [`rust/auth/verify/clients/16/rotate time`](https://kixel.quest/factory/series/18323?repository=508763694), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-28: 12.71 % against a baseline of 5 %, +7.709 % (+154.2%), at [`f101d79`](https://github.com/moq-dev/moq/commit/f101d7908e28dc257311dc4502d4919b1d4c75a7). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 0.0016.

### `rust/auth/verify/decide/HS256/dir time`: flake

<!-- kixel.quest finding {"series":"rust/auth/verify/decide/HS256/dir time","culprit":null} -->

- Series: [`rust/auth/verify/decide/HS256/dir time`](https://kixel.quest/factory/series/18341?repository=508763694), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-28: 20.69 % against a baseline of 5 %, +15.69 % (+313.7%), at [`f101d79`](https://github.com/moq-dev/moq/commit/f101d7908e28dc257311dc4502d4919b1d4c75a7). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 3.7e-8.

### `rust/auth/verify/decide/HS256/file time`: flake

<!-- kixel.quest finding {"series":"rust/auth/verify/decide/HS256/file time","culprit":null} -->

- Series: [`rust/auth/verify/decide/HS256/file time`](https://kixel.quest/factory/series/18342?repository=508763694), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-28: 12.85 % against a baseline of 5 %, +7.853 % (+157.1%), at [`f101d79`](https://github.com/moq-dev/moq/commit/f101d7908e28dc257311dc4502d4919b1d4c75a7). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 0.0013.

### `rust/auth/verify/decide/HS384/file time`: flake

<!-- kixel.quest finding {"series":"rust/auth/verify/decide/HS384/file time","culprit":null} -->

- Series: [`rust/auth/verify/decide/HS384/file time`](https://kixel.quest/factory/series/18345?repository=508763694), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-28: 19.74 % against a baseline of 5 %, +14.74 % (+294.8%), at [`f101d79`](https://github.com/moq-dev/moq/commit/f101d7908e28dc257311dc4502d4919b1d4c75a7). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 1.7e-7.

### `rust/auth/verify/decide/HS384/set time`: flake

<!-- kixel.quest finding {"series":"rust/auth/verify/decide/HS384/set time","culprit":null} -->

- Series: [`rust/auth/verify/decide/HS384/set time`](https://kixel.quest/factory/series/18346?repository=508763694), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-28: 20.66 % against a baseline of 5 %, +15.66 % (+313.2%), at [`f101d79`](https://github.com/moq-dev/moq/commit/f101d7908e28dc257311dc4502d4919b1d4c75a7). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 3.9e-8.

### `rust/auth/verify/decide/HS512/dir time`: flake

<!-- kixel.quest finding {"series":"rust/auth/verify/decide/HS512/dir time","culprit":null} -->

- Series: [`rust/auth/verify/decide/HS512/dir time`](https://kixel.quest/factory/series/18347?repository=508763694), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-28: 16.85 % against a baseline of 5 %, +11.85 % (+236.9%), at [`f101d79`](https://github.com/moq-dev/moq/commit/f101d7908e28dc257311dc4502d4919b1d4c75a7). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 0.000012.

### `rust/auth/verify/decide/HS512/file time`: flake

<!-- kixel.quest finding {"series":"rust/auth/verify/decide/HS512/file time","culprit":null} -->

- Series: [`rust/auth/verify/decide/HS512/file time`](https://kixel.quest/factory/series/18348?repository=508763694), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-28: 20.43 % against a baseline of 5 %, +15.43 % (+308.5%), at [`f101d79`](https://github.com/moq-dev/moq/commit/f101d7908e28dc257311dc4502d4919b1d4c75a7). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 5.7e-8.

### `rust/auth/verify/decide/PS256/dir time`: flake

<!-- kixel.quest finding {"series":"rust/auth/verify/decide/PS256/dir time","culprit":null} -->

- Series: [`rust/auth/verify/decide/PS256/dir time`](https://kixel.quest/factory/series/18350?repository=508763694), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-28: 13.42 % against a baseline of 5 %, +8.418 % (+168.4%), at [`f101d79`](https://github.com/moq-dev/moq/commit/f101d7908e28dc257311dc4502d4919b1d4c75a7). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 0.00074.

### `rust/auth/verify/decide/PS256/file time`: flake

<!-- kixel.quest finding {"series":"rust/auth/verify/decide/PS256/file time","culprit":null} -->

- Series: [`rust/auth/verify/decide/PS256/file time`](https://kixel.quest/factory/series/18351?repository=508763694), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-28: 16.8 % against a baseline of 5 %, +11.8 % (+236%), at [`f101d79`](https://github.com/moq-dev/moq/commit/f101d7908e28dc257311dc4502d4919b1d4c75a7). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 0.000013.

### `rust/auth/verify/decide/PS384/dir time`: flake

<!-- kixel.quest finding {"series":"rust/auth/verify/decide/PS384/dir time","culprit":null} -->

- Series: [`rust/auth/verify/decide/PS384/dir time`](https://kixel.quest/factory/series/18353?repository=508763694), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-28: 13.88 % against a baseline of 5 %, +8.881 % (+177.6%), at [`f101d79`](https://github.com/moq-dev/moq/commit/f101d7908e28dc257311dc4502d4919b1d4c75a7). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 0.00045.

### `rust/auth/verify/decide/PS384/file time`: flake

<!-- kixel.quest finding {"series":"rust/auth/verify/decide/PS384/file time","culprit":null} -->

- Series: [`rust/auth/verify/decide/PS384/file time`](https://kixel.quest/factory/series/18354?repository=508763694), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-28: 14.12 % against a baseline of 5 %, +9.119 % (+182.4%), at [`f101d79`](https://github.com/moq-dev/moq/commit/f101d7908e28dc257311dc4502d4919b1d4c75a7). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 0.00034.

### `rust/auth/verify/decide/PS384/set time`: flake

<!-- kixel.quest finding {"series":"rust/auth/verify/decide/PS384/set time","culprit":null} -->

- Series: [`rust/auth/verify/decide/PS384/set time`](https://kixel.quest/factory/series/18355?repository=508763694), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-28: 15.53 % against a baseline of 5 %, +10.53 % (+210.7%), at [`f101d79`](https://github.com/moq-dev/moq/commit/f101d7908e28dc257311dc4502d4919b1d4c75a7). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 0.000064.

### `rust/auth/verify/decide/PS512/dir time`: flake

<!-- kixel.quest finding {"series":"rust/auth/verify/decide/PS512/dir time","culprit":null} -->

- Series: [`rust/auth/verify/decide/PS512/dir time`](https://kixel.quest/factory/series/18356?repository=508763694), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-28: 10.98 % against a baseline of 5 %, +5.977 % (+119.5%), at [`f101d79`](https://github.com/moq-dev/moq/commit/f101d7908e28dc257311dc4502d4919b1d4c75a7). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 0.0081.

### `rust/auth/verify/decide/PS512/set time`: flake

<!-- kixel.quest finding {"series":"rust/auth/verify/decide/PS512/set time","culprit":null} -->

- Series: [`rust/auth/verify/decide/PS512/set time`](https://kixel.quest/factory/series/18358?repository=508763694), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-28: 11.32 % against a baseline of 5 %, +6.315 % (+126.3%), at [`f101d79`](https://github.com/moq-dev/moq/commit/f101d7908e28dc257311dc4502d4919b1d4c75a7). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 0.0060.

### `rust/auth/verify/decide/RS256/dir time`: flake

<!-- kixel.quest finding {"series":"rust/auth/verify/decide/RS256/dir time","culprit":null} -->

- Series: [`rust/auth/verify/decide/RS256/dir time`](https://kixel.quest/factory/series/18359?repository=508763694), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-28: 12.18 % against a baseline of 5 %, +7.18 % (+143.6%), at [`f101d79`](https://github.com/moq-dev/moq/commit/f101d7908e28dc257311dc4502d4919b1d4c75a7). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 0.0026.

### `rust/auth/verify/decide/RS256/file time`: flake

<!-- kixel.quest finding {"series":"rust/auth/verify/decide/RS256/file time","culprit":null} -->

- Series: [`rust/auth/verify/decide/RS256/file time`](https://kixel.quest/factory/series/18360?repository=508763694), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-28: 11.93 % against a baseline of 5 %, +6.926 % (+138.5%), at [`f101d79`](https://github.com/moq-dev/moq/commit/f101d7908e28dc257311dc4502d4919b1d4c75a7). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 0.0034.

### `rust/auth/verify/decide/RS256/set time`: flake

<!-- kixel.quest finding {"series":"rust/auth/verify/decide/RS256/set time","culprit":null} -->

- Series: [`rust/auth/verify/decide/RS256/set time`](https://kixel.quest/factory/series/18361?repository=508763694), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-28: 13.67 % against a baseline of 5 %, +8.672 % (+173.4%), at [`f101d79`](https://github.com/moq-dev/moq/commit/f101d7908e28dc257311dc4502d4919b1d4c75a7). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 0.00057.

### `rust/auth/verify/decide/RS384/file time`: flake

<!-- kixel.quest finding {"series":"rust/auth/verify/decide/RS384/file time","culprit":null} -->

- Series: [`rust/auth/verify/decide/RS384/file time`](https://kixel.quest/factory/series/18363?repository=508763694), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-28: 13.1 % against a baseline of 5 %, +8.101 % (+162%), at [`f101d79`](https://github.com/moq-dev/moq/commit/f101d7908e28dc257311dc4502d4919b1d4c75a7). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 0.0010.

### `rust/auth/verify/decide/RS512/dir time`: flake

<!-- kixel.quest finding {"series":"rust/auth/verify/decide/RS512/dir time","culprit":null} -->

- Series: [`rust/auth/verify/decide/RS512/dir time`](https://kixel.quest/factory/series/18365?repository=508763694), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-28: 13.81 % against a baseline of 5 %, +8.813 % (+176.3%), at [`f101d79`](https://github.com/moq-dev/moq/commit/f101d7908e28dc257311dc4502d4919b1d4c75a7). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 0.00048.

### `rust/auth/verify/decide/RS512/file time`: flake

<!-- kixel.quest finding {"series":"rust/auth/verify/decide/RS512/file time","culprit":null} -->

- Series: [`rust/auth/verify/decide/RS512/file time`](https://kixel.quest/factory/series/18366?repository=508763694), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-28: 10.91 % against a baseline of 5 %, +5.91 % (+118.2%), at [`f101d79`](https://github.com/moq-dev/moq/commit/f101d7908e28dc257311dc4502d4919b1d4c75a7). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 0.0086.

### `rust/auth/verify/decide/RS512/set time`: flake

<!-- kixel.quest finding {"series":"rust/auth/verify/decide/RS512/set time","culprit":null} -->

- Series: [`rust/auth/verify/decide/RS512/set time`](https://kixel.quest/factory/series/18367?repository=508763694), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-28: 12.64 % against a baseline of 5 %, +7.636 % (+152.7%), at [`f101d79`](https://github.com/moq-dev/moq/commit/f101d7908e28dc257311dc4502d4919b1d4c75a7). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 0.0017.

### `rust/auth/verify/http/HS256 time`: flake

<!-- kixel.quest finding {"series":"rust/auth/verify/http/HS256 time","culprit":null} -->

- Series: [`rust/auth/verify/http/HS256 time`](https://kixel.quest/factory/series/18369?repository=508763694), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-28: 16.69 % against a baseline of 5 %, +11.69 % (+233.8%), at [`f101d79`](https://github.com/moq-dev/moq/commit/f101d7908e28dc257311dc4502d4919b1d4c75a7). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 0.000014.

### `rust/auth/verify/refuse/unknown_kid time`: flake

<!-- kixel.quest finding {"series":"rust/auth/verify/refuse/unknown_kid time","culprit":null} -->

- Series: [`rust/auth/verify/refuse/unknown_kid time`](https://kixel.quest/factory/series/18374?repository=508763694), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-28: 23.37 % against a baseline of 5 %, +18.37 % (+367.3%), at [`f101d79`](https://github.com/moq-dev/moq/commit/f101d7908e28dc257311dc4502d4919b1d4c75a7). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 3.3e-10.

### `rust/auth/verify/sessions/0 time`: flake

<!-- kixel.quest finding {"series":"rust/auth/verify/sessions/0 time","culprit":null} -->

- Series: [`rust/auth/verify/sessions/0 time`](https://kixel.quest/factory/series/18375?repository=508763694), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-28: 16.07 % against a baseline of 5 %, +11.07 % (+221.3%), at [`f101d79`](https://github.com/moq-dev/moq/commit/f101d7908e28dc257311dc4502d4919b1d4c75a7). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 0.000033.

### `rust/auth/verify/sessions/1024 time`: flake

<!-- kixel.quest finding {"series":"rust/auth/verify/sessions/1024 time","culprit":null} -->

- Series: [`rust/auth/verify/sessions/1024 time`](https://kixel.quest/factory/series/18376?repository=508763694), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-28: 17.96 % against a baseline of 5 %, +12.96 % (+259.3%), at [`f101d79`](https://github.com/moq-dev/moq/commit/f101d7908e28dc257311dc4502d4919b1d4c75a7). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 0.0000025.

### `rust/auth/verify/sessions/16384 time`: flake

<!-- kixel.quest finding {"series":"rust/auth/verify/sessions/16384 time","culprit":null} -->

- Series: [`rust/auth/verify/sessions/16384 time`](https://kixel.quest/factory/series/18377?repository=508763694), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-28: 12.23 % against a baseline of 5 %, +7.234 % (+144.7%), at [`f101d79`](https://github.com/moq-dev/moq/commit/f101d7908e28dc257311dc4502d4919b1d4c75a7). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 0.0025.

### `rust/auth/verify/set/256/missing time`: flake

<!-- kixel.quest finding {"series":"rust/auth/verify/set/256/missing time","culprit":null} -->

- Series: [`rust/auth/verify/set/256/missing time`](https://kixel.quest/factory/series/18386?repository=508763694), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-28: 16.93 % against a baseline of 5 %, +11.93 % (+238.7%), at [`f101d79`](https://github.com/moq-dev/moq/commit/f101d7908e28dc257311dc4502d4919b1d4c75a7). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 0.000010.

### `rust/auth/verify/stage/EdDSA/import time`: flake

<!-- kixel.quest finding {"series":"rust/auth/verify/stage/EdDSA/import time","culprit":null} -->

- Series: [`rust/auth/verify/stage/EdDSA/import time`](https://kixel.quest/factory/series/18395?repository=508763694), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-28: 46.12 % against a baseline of 5 %, +41.12 % (+822.3%), at [`f101d79`](https://github.com/moq-dev/moq/commit/f101d7908e28dc257311dc4502d4919b1d4c75a7). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 1.1e-37.

### `rust/auth/verify/stage/read_tokio time`: flake

<!-- kixel.quest finding {"series":"rust/auth/verify/stage/read_tokio time","culprit":null} -->

- Series: [`rust/auth/verify/stage/read_tokio time`](https://kixel.quest/factory/series/18443?repository=508763694), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-28: 11.72 % against a baseline of 5 %, +6.724 % (+134.5%), at [`f101d79`](https://github.com/moq-dev/moq/commit/f101d7908e28dc257311dc4502d4919b1d4c75a7). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 0.0041.

### `rust/extra/fanout/uni_1KiB/1 time`: flake

<!-- kixel.quest finding {"series":"rust/extra/fanout/uni_1KiB/1 time","culprit":null} -->

- Series: [`rust/extra/fanout/uni_1KiB/1 time`](https://kixel.quest/factory/series/18268?repository=508763694), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-28: 22.85 % against a baseline of 5 %, +17.85 % (+357.1%), at [`f101d79`](https://github.com/moq-dev/moq/commit/f101d7908e28dc257311dc4502d4919b1d4c75a7). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 8.4e-10.

### `rust/extra/fanout/uni_1KiB/16 time`: flake

<!-- kixel.quest finding {"series":"rust/extra/fanout/uni_1KiB/16 time","culprit":null} -->

- Series: [`rust/extra/fanout/uni_1KiB/16 time`](https://kixel.quest/factory/series/18270?repository=508763694), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-28: 11.37 % against a baseline of 5 %, +6.374 % (+127.5%), at [`f101d79`](https://github.com/moq-dev/moq/commit/f101d7908e28dc257311dc4502d4919b1d4c75a7). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 0.0057.

### `rust/extra/pcm_batches/pcm/48000hz/2ch/batch_480/producers_8 time`: flake

<!-- kixel.quest finding {"series":"rust/extra/pcm_batches/pcm/48000hz/2ch/batch_480/producers_8 time","culprit":null} -->

- Series: [`rust/extra/pcm_batches/pcm/48000hz/2ch/batch_480/producers_8 time`](https://kixel.quest/factory/series/17975?repository=508763694), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-28: 12.95 % against a baseline of 5 %, +7.948 % (+159%), at [`f101d79`](https://github.com/moq-dev/moq/commit/f101d7908e28dc257311dc4502d4919b1d4c75a7). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 0.0012.

### `rust/extra/receive_bursts/cancel/streams_256/packets_256 time`: flake

<!-- kixel.quest finding {"series":"rust/extra/receive_bursts/cancel/streams_256/packets_256 time","culprit":null} -->

- Series: [`rust/extra/receive_bursts/cancel/streams_256/packets_256 time`](https://kixel.quest/factory/series/18238?repository=508763694), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-28: 17.41 % against a baseline of 5 %, +12.41 % (+248.1%), at [`f101d79`](https://github.com/moq-dev/moq/commit/f101d7908e28dc257311dc4502d4919b1d4c75a7). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 0.0000055.

### `rust/extra/receive_bursts/ordered/streams_256/packets_256 time`: flake

<!-- kixel.quest finding {"series":"rust/extra/receive_bursts/ordered/streams_256/packets_256 time","culprit":null} -->

- Series: [`rust/extra/receive_bursts/ordered/streams_256/packets_256 time`](https://kixel.quest/factory/series/18256?repository=508763694), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-28: 14.31 % against a baseline of 5 %, +9.314 % (+186.3%), at [`f101d79`](https://github.com/moq-dev/moq/commit/f101d7908e28dc257311dc4502d4919b1d4c75a7). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 0.00028.

### `rust/extra/table/deltas/1%/20000 time`: flake

<!-- kixel.quest finding {"series":"rust/extra/table/deltas/1%/20000 time","culprit":null} -->

- Series: [`rust/extra/table/deltas/1%/20000 time`](https://kixel.quest/factory/series/18062?repository=508763694), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-28: 16.94 % against a baseline of 5 %, +11.94 % (+238.7%), at [`f101d79`](https://github.com/moq-dev/moq/commit/f101d7908e28dc257311dc4502d4919b1d4c75a7). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 0.000010.

### `rust/extra/table/deltas/10%/10000 time`: flake

<!-- kixel.quest finding {"series":"rust/extra/table/deltas/10%/10000 time","culprit":null} -->

- Series: [`rust/extra/table/deltas/10%/10000 time`](https://kixel.quest/factory/series/18065?repository=508763694), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-28: 11.15 % against a baseline of 5 %, +6.146 % (+122.9%), at [`f101d79`](https://github.com/moq-dev/moq/commit/f101d7908e28dc257311dc4502d4919b1d4c75a7). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 0.0069.

### `rust/extra/table/deltas/10%/20000 time`: flake

<!-- kixel.quest finding {"series":"rust/extra/table/deltas/10%/20000 time","culprit":null} -->

- Series: [`rust/extra/table/deltas/10%/20000 time`](https://kixel.quest/factory/series/18066?repository=508763694), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-28: 20.66 % against a baseline of 5 %, +15.66 % (+313.3%), at [`f101d79`](https://github.com/moq-dev/moq/commit/f101d7908e28dc257311dc4502d4919b1d4c75a7). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 3.8e-8.

### `rust/extra/table/deltas/100%/10000 time`: flake

<!-- kixel.quest finding {"series":"rust/extra/table/deltas/100%/10000 time","culprit":null} -->

- Series: [`rust/extra/table/deltas/100%/10000 time`](https://kixel.quest/factory/series/18069?repository=508763694), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-28: 15.56 % against a baseline of 5 %, +10.56 % (+211.2%), at [`f101d79`](https://github.com/moq-dev/moq/commit/f101d7908e28dc257311dc4502d4919b1d4c75a7). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 0.000062.

### `rust/extra/table/deltas/100%/20000 time`: flake

<!-- kixel.quest finding {"series":"rust/extra/table/deltas/100%/20000 time","culprit":null} -->

- Series: [`rust/extra/table/deltas/100%/20000 time`](https://kixel.quest/factory/series/18070?repository=508763694), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-28: 12.29 % against a baseline of 5 %, +7.292 % (+145.8%), at [`f101d79`](https://github.com/moq-dev/moq/commit/f101d7908e28dc257311dc4502d4919b1d4c75a7). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 0.0024.

### `rust/extra/ts_video/4096/65000 time`: flake

<!-- kixel.quest finding {"series":"rust/extra/ts_video/4096/65000 time","culprit":null} -->

- Series: [`rust/extra/ts_video/4096/65000 time`](https://kixel.quest/factory/series/18226?repository=508763694), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-28: 19.27 % against a baseline of 5 %, +14.27 % (+285.3%), at [`f101d79`](https://github.com/moq-dev/moq/commit/f101d7908e28dc257311dc4502d4919b1d4c75a7). Spread as two pooled standard deviations from each commit's mean, over 4 observations of 2 commits, against the 5% regression it must detect; chi-square test p = 3.6e-7.

### `rust/shared/cmaf-export-frames/1tracks-1frames/256 time`: flake

<!-- kixel.quest finding {"series":"rust/shared/cmaf\u002dexport\u002dframes/1tracks\u002d1frames/256 time","culprit":null} -->

- Series: [`rust/shared/cmaf-export-frames/1tracks-1frames/256 time`](https://kixel.quest/factory/series/18946?repository=508763694), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-28: 9.8 % against a baseline of 5 %, +4.8 % (+96.01%), at [`f101d79`](https://github.com/moq-dev/moq/commit/f101d7908e28dc257311dc4502d4919b1d4c75a7). Spread as two pooled standard deviations from each commit's mean, over 8 observations of 4 commits, against the 5% regression it must detect; chi-square test p = 0.0040.

### `rust/shared/cmaf-export-frames/1tracks-1frames/4096 time`: flake

<!-- kixel.quest finding {"series":"rust/shared/cmaf\u002dexport\u002dframes/1tracks\u002d1frames/4096 time","culprit":null} -->

- Series: [`rust/shared/cmaf-export-frames/1tracks-1frames/4096 time`](https://kixel.quest/factory/series/18947?repository=508763694), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-28: 9.128 % against a baseline of 5 %, +4.128 % (+82.56%), at [`f101d79`](https://github.com/moq-dev/moq/commit/f101d7908e28dc257311dc4502d4919b1d4c75a7). Spread as two pooled standard deviations from each commit's mean, over 8 observations of 4 commits, against the 5% regression it must detect; chi-square test p = 0.0098.

### `rust/shared/relay_auth_queue/1/16 time`: flake

<!-- kixel.quest finding {"series":"rust/shared/relay_auth_queue/1/16 time","culprit":null} -->

- Series: [`rust/shared/relay_auth_queue/1/16 time`](https://kixel.quest/factory/series/19437?repository=508763694), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-28: 16.11 % against a baseline of 5 %, +11.11 % (+222.2%), at [`f101d79`](https://github.com/moq-dev/moq/commit/f101d7908e28dc257311dc4502d4919b1d4c75a7). Spread as two pooled standard deviations from each commit's mean, over 8 observations of 4 commits, against the 5% regression it must detect; chi-square test p = 2.1e-8.

### `rust/shared/relay_auth_queue/1/256 time`: flake

<!-- kixel.quest finding {"series":"rust/shared/relay_auth_queue/1/256 time","culprit":null} -->

- Series: [`rust/shared/relay_auth_queue/1/256 time`](https://kixel.quest/factory/series/19438?repository=508763694), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-28: 12.41 % against a baseline of 5 %, +7.412 % (+148.2%), at [`f101d79`](https://github.com/moq-dev/moq/commit/f101d7908e28dc257311dc4502d4919b1d4c75a7). Spread as two pooled standard deviations from each commit's mean, over 8 observations of 4 commits, against the 5% regression it must detect; chi-square test p = 0.000059.

### `rust/shared/relay_auth_refresh/1/1 time`: flake

<!-- kixel.quest finding {"series":"rust/shared/relay_auth_refresh/1/1 time","culprit":null} -->

- Series: [`rust/shared/relay_auth_refresh/1/1 time`](https://kixel.quest/factory/series/19446?repository=508763694), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-28: 11.15 % against a baseline of 5 %, +6.15 % (+123%), at [`f101d79`](https://github.com/moq-dev/moq/commit/f101d7908e28dc257311dc4502d4919b1d4c75a7). Spread as two pooled standard deviations from each commit's mean, over 8 observations of 4 commits, against the 5% regression it must detect; chi-square test p = 0.00052.

### `rust/shared/relay_auth_refresh/64/16 time`: flake

<!-- kixel.quest finding {"series":"rust/shared/relay_auth_refresh/64/16 time","culprit":null} -->

- Series: [`rust/shared/relay_auth_refresh/64/16 time`](https://kixel.quest/factory/series/19453?repository=508763694), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-28: 12.5 % against a baseline of 5 %, +7.505 % (+150.1%), at [`f101d79`](https://github.com/moq-dev/moq/commit/f101d7908e28dc257311dc4502d4919b1d4c75a7). Spread as two pooled standard deviations from each commit's mean, over 8 observations of 4 commits, against the 5% regression it must detect; chi-square test p = 0.000050.

### `rust/shared/relay_http_admission/connections=1/0 time`: flake

<!-- kixel.quest finding {"series":"rust/shared/relay_http_admission/connections=1/0 time","culprit":null} -->

- Series: [`rust/shared/relay_http_admission/connections=1/0 time`](https://kixel.quest/factory/series/19455?repository=508763694), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-28: 14.05 % against a baseline of 5 %, +9.05 % (+181%), at [`f101d79`](https://github.com/moq-dev/moq/commit/f101d7908e28dc257311dc4502d4919b1d4c75a7). Spread as two pooled standard deviations from each commit's mean, over 8 observations of 4 commits, against the 5% regression it must detect; chi-square test p = 0.0000023.

### `rust/shared/relay_http_admission/connections=1/16 time`: flake

<!-- kixel.quest finding {"series":"rust/shared/relay_http_admission/connections=1/16 time","culprit":null} -->

- Series: [`rust/shared/relay_http_admission/connections=1/16 time`](https://kixel.quest/factory/series/19456?repository=508763694), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-28: 13.49 % against a baseline of 5 %, +8.488 % (+169.8%), at [`f101d79`](https://github.com/moq-dev/moq/commit/f101d7908e28dc257311dc4502d4919b1d4c75a7). Spread as two pooled standard deviations from each commit's mean, over 8 observations of 4 commits, against the 5% regression it must detect; chi-square test p = 0.0000074.

### `rust/shared/relay_http_admission/connections=16/0 time`: flake

<!-- kixel.quest finding {"series":"rust/shared/relay_http_admission/connections=16/0 time","culprit":null} -->

- Series: [`rust/shared/relay_http_admission/connections=16/0 time`](https://kixel.quest/factory/series/19458?repository=508763694), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-28: 17.16 % against a baseline of 5 %, +12.16 % (+243.1%), at [`f101d79`](https://github.com/moq-dev/moq/commit/f101d7908e28dc257311dc4502d4919b1d4c75a7). Spread as two pooled standard deviations from each commit's mean, over 8 observations of 4 commits, against the 5% regression it must detect; chi-square test p = 1.5e-9.

### `rust/shared/relay_http_admission/connections=64/0 time`: flake

<!-- kixel.quest finding {"series":"rust/shared/relay_http_admission/connections=64/0 time","culprit":null} -->

- Series: [`rust/shared/relay_http_admission/connections=64/0 time`](https://kixel.quest/factory/series/19461?repository=508763694), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-28: 18.2 % against a baseline of 5 %, +13.2 % (+263.9%), at [`f101d79`](https://github.com/moq-dev/moq/commit/f101d7908e28dc257311dc4502d4919b1d4c75a7). Spread as two pooled standard deviations from each commit's mean, over 8 observations of 4 commits, against the 5% regression it must detect; chi-square test p = 8.6e-11.

### `rust/shared/relay_http_admission/connections=64/16 time`: flake

<!-- kixel.quest finding {"series":"rust/shared/relay_http_admission/connections=64/16 time","culprit":null} -->

- Series: [`rust/shared/relay_http_admission/connections=64/16 time`](https://kixel.quest/factory/series/19462?repository=508763694), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-28: 15.38 % against a baseline of 5 %, +10.38 % (+207.6%), at [`f101d79`](https://github.com/moq-dev/moq/commit/f101d7908e28dc257311dc4502d4919b1d4c75a7). Spread as two pooled standard deviations from each commit's mean, over 8 observations of 4 commits, against the 5% regression it must detect; chi-square test p = 1.2e-7.

### `rust/shared/relay_http_admission/connections=64/64 time`: flake

<!-- kixel.quest finding {"series":"rust/shared/relay_http_admission/connections=64/64 time","culprit":null} -->

- Series: [`rust/shared/relay_http_admission/connections=64/64 time`](https://kixel.quest/factory/series/19463?repository=508763694), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-28: 12.31 % against a baseline of 5 %, +7.309 % (+146.2%), at [`f101d79`](https://github.com/moq-dev/moq/commit/f101d7908e28dc257311dc4502d4919b1d4c75a7). Spread as two pooled standard deviations from each commit's mean, over 8 observations of 4 commits, against the 5% regression it must detect; chi-square test p = 0.000071.

### `rust/shared/relay_http_auth_server/connections=1/0 time`: flake

<!-- kixel.quest finding {"series":"rust/shared/relay_http_auth_server/connections=1/0 time","culprit":null} -->

- Series: [`rust/shared/relay_http_auth_server/connections=1/0 time`](https://kixel.quest/factory/series/19464?repository=508763694), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-28: 12 % against a baseline of 5 %, +7.003 % (+140.1%), at [`f101d79`](https://github.com/moq-dev/moq/commit/f101d7908e28dc257311dc4502d4919b1d4c75a7). Spread as two pooled standard deviations from each commit's mean, over 8 observations of 4 commits, against the 5% regression it must detect; chi-square test p = 0.00012.

### `rust/shared/relay_http_auth_server/connections=1/4 time`: flake

<!-- kixel.quest finding {"series":"rust/shared/relay_http_auth_server/connections=1/4 time","culprit":null} -->

- Series: [`rust/shared/relay_http_auth_server/connections=1/4 time`](https://kixel.quest/factory/series/19466?repository=508763694), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-28: 10.37 % against a baseline of 5 %, +5.371 % (+107.4%), at [`f101d79`](https://github.com/moq-dev/moq/commit/f101d7908e28dc257311dc4502d4919b1d4c75a7). Spread as two pooled standard deviations from each commit's mean, over 8 observations of 4 commits, against the 5% regression it must detect; chi-square test p = 0.0018.

### `rust/shared/relay_http_auth_server/connections=4/0 time`: flake

<!-- kixel.quest finding {"series":"rust/shared/relay_http_auth_server/connections=4/0 time","culprit":null} -->

- Series: [`rust/shared/relay_http_auth_server/connections=4/0 time`](https://kixel.quest/factory/series/19470?repository=508763694), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-28: 12.24 % against a baseline of 5 %, +7.243 % (+144.9%), at [`f101d79`](https://github.com/moq-dev/moq/commit/f101d7908e28dc257311dc4502d4919b1d4c75a7). Spread as two pooled standard deviations from each commit's mean, over 8 observations of 4 commits, against the 5% regression it must detect; chi-square test p = 0.000080.

### `rust/shared/relay_http_auth_server/connections=4/16 time`: flake

<!-- kixel.quest finding {"series":"rust/shared/relay_http_auth_server/connections=4/16 time","culprit":null} -->

- Series: [`rust/shared/relay_http_auth_server/connections=4/16 time`](https://kixel.quest/factory/series/19471?repository=508763694), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-28: 9.939 % against a baseline of 5 %, +4.939 % (+98.78%), at [`f101d79`](https://github.com/moq-dev/moq/commit/f101d7908e28dc257311dc4502d4919b1d4c75a7). Spread as two pooled standard deviations from each commit's mean, over 8 observations of 4 commits, against the 5% regression it must detect; chi-square test p = 0.0033.

### `rust/shared/relay_http_auth_server/connections=4/4 time`: flake

<!-- kixel.quest finding {"series":"rust/shared/relay_http_auth_server/connections=4/4 time","culprit":null} -->

- Series: [`rust/shared/relay_http_auth_server/connections=4/4 time`](https://kixel.quest/factory/series/19472?repository=508763694), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-28: 11.5 % against a baseline of 5 %, +6.504 % (+130.1%), at [`f101d79`](https://github.com/moq-dev/moq/commit/f101d7908e28dc257311dc4502d4919b1d4c75a7). Spread as two pooled standard deviations from each commit's mean, over 8 observations of 4 commits, against the 5% regression it must detect; chi-square test p = 0.00029.

### `rust/shared/schedule/pop/groups_256_queued_256 time`: flake

<!-- kixel.quest finding {"series":"rust/shared/schedule/pop/groups_256_queued_256 time","culprit":null} -->

- Series: [`rust/shared/schedule/pop/groups_256_queued_256 time`](https://kixel.quest/factory/series/19384?repository=508763694), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-28: 11.2 % against a baseline of 5 %, +6.204 % (+124.1%), at [`f101d79`](https://github.com/moq-dev/moq/commit/f101d7908e28dc257311dc4502d4919b1d4c75a7). Spread as two pooled standard deviations from each commit's mean, over 8 observations of 4 commits, against the 5% regression it must detect; chi-square test p = 0.00048.

### `rust/shared/ts_si_pending_pcr/64/1 time`: flake

<!-- kixel.quest finding {"series":"rust/shared/ts_si_pending_pcr/64/1 time","culprit":null} -->

- Series: [`rust/shared/ts_si_pending_pcr/64/1 time`](https://kixel.quest/factory/series/19078?repository=508763694), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-28: 21.77 % against a baseline of 5 %, +16.77 % (+335.3%), at [`f101d79`](https://github.com/moq-dev/moq/commit/f101d7908e28dc257311dc4502d4919b1d4c75a7). Spread as two pooled standard deviations from each commit's mean, over 8 observations of 4 commits, against the 5% regression it must detect; chi-square test p = 1.3e-15.

### `rust/shared/ts_si_table_history_pcr/1/1 time`: flake

<!-- kixel.quest finding {"series":"rust/shared/ts_si_table_history_pcr/1/1 time","culprit":null} -->

- Series: [`rust/shared/ts_si_table_history_pcr/1/1 time`](https://kixel.quest/factory/series/19090?repository=508763694), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-28: 15.19 % against a baseline of 5 %, +10.19 % (+203.9%), at [`f101d79`](https://github.com/moq-dev/moq/commit/f101d7908e28dc257311dc4502d4919b1d4c75a7). Spread as two pooled standard deviations from each commit's mean, over 8 observations of 4 commits, against the 5% regression it must detect; chi-square test p = 1.9e-7.

### `rust/shared/ts_si_table_history_pcr/64/1 time`: flake

<!-- kixel.quest finding {"series":"rust/shared/ts_si_table_history_pcr/64/1 time","culprit":null} -->

- Series: [`rust/shared/ts_si_table_history_pcr/64/1 time`](https://kixel.quest/factory/series/19096?repository=508763694), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-28: 20.5 % against a baseline of 5 %, +15.5 % (+309.9%), at [`f101d79`](https://github.com/moq-dev/moq/commit/f101d7908e28dc257311dc4502d4919b1d4c75a7). Spread as two pooled standard deviations from each commit's mean, over 8 observations of 4 commits, against the 5% regression it must detect; chi-square test p = 8.8e-14.

### `rust/shared/video-reorder/pyramid/64x16384 time`: flake

<!-- kixel.quest finding {"series":"rust/shared/video\u002dreorder/pyramid/64x16384 time","culprit":null} -->

- Series: [`rust/shared/video-reorder/pyramid/64x16384 time`](https://kixel.quest/factory/series/19151?repository=508763694), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-28: 16.62 % against a baseline of 5 %, +11.62 % (+232.5%), at [`f101d79`](https://github.com/moq-dev/moq/commit/f101d7908e28dc257311dc4502d4919b1d4c75a7). Spread as two pooled standard deviations from each commit's mean, over 8 observations of 4 commits, against the 5% regression it must detect; chi-square test p = 5.8e-9.

### `rust/shared/worker_turns/sockets-16/1 time`: flake

<!-- kixel.quest finding {"series":"rust/shared/worker_turns/sockets\u002d16/1 time","culprit":null} -->

- Series: [`rust/shared/worker_turns/sockets-16/1 time`](https://kixel.quest/factory/series/19608?repository=508763694), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-28: 14.14 % against a baseline of 5 %, +9.142 % (+182.8%), at [`f101d79`](https://github.com/moq-dev/moq/commit/f101d7908e28dc257311dc4502d4919b1d4c75a7). Spread as two pooled standard deviations from each commit's mean, over 8 observations of 4 commits, against the 5% regression it must detect; chi-square test p = 0.0000019.

### `rust/shared/worker_turns/sockets-4/8 time`: flake

<!-- kixel.quest finding {"series":"rust/shared/worker_turns/sockets\u002d4/8 time","culprit":null} -->

- Series: [`rust/shared/worker_turns/sockets-4/8 time`](https://kixel.quest/factory/series/19613?repository=508763694), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-28: 10.19 % against a baseline of 5 %, +5.191 % (+103.8%), at [`f101d79`](https://github.com/moq-dev/moq/commit/f101d7908e28dc257311dc4502d4919b1d4c75a7). Spread as two pooled standard deviations from each commit's mean, over 8 observations of 4 commits, against the 5% regression it must detect; chi-square test p = 0.0023.

### `rust/uring-session_lite/session_lite/all-off time`: flake

<!-- kixel.quest finding {"series":"rust/uring\u002dsession_lite/session_lite/all\u002doff time","culprit":null} -->

- Series: [`rust/uring-session_lite/session_lite/all-off time`](https://kixel.quest/factory/series/19760?repository=508763694), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-28: 14.03 % against a baseline of 5 %, +9.03 % (+180.6%), at [`f101d79`](https://github.com/moq-dev/moq/commit/f101d7908e28dc257311dc4502d4919b1d4c75a7). Spread as two pooled standard deviations from each commit's mean, over 8 observations of 4 commits, against the 5% regression it must detect; chi-square test p = 0.0000024.

### `rust/uring-web_headers/web_headers/pending-64/ready-1 time`: flake

<!-- kixel.quest finding {"series":"rust/uring\u002dweb_headers/web_headers/pending\u002d64/ready\u002d1 time","culprit":null} -->

- Series: [`rust/uring-web_headers/web_headers/pending-64/ready-1 time`](https://kixel.quest/factory/series/19782?repository=508763694), in %.
- A flake: fix it at the root cause now, or disable it until it is fixed and record that here. Never retry it, wait it out, or loosen what it checks. This quest stays open until it is fixed and enabled again.
- 2026-11-28: 9.651 % against a baseline of 5 %, +4.651 % (+93.03%), at [`f101d79`](https://github.com/moq-dev/moq/commit/f101d7908e28dc257311dc4502d4919b1d4c75a7). Spread as two pooled standard deviations from each commit's mean, over 8 observations of 4 commits, against the 5% regression it must detect; chi-square test p = 0.0049.
