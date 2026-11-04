# Nightly artifact sizes

`just gh size-collect OUTPUT [RUN]` measures what a workflow run's release jobs
uploaded, plus the builds no release job makes: the minimal-feature FFI library
and real browser consumer bundles (raw, gzip and Brotli). `just gh size-report
OUTPUT [--history HISTORY]` compares the report with its previous report (5%
growth) and the newest report at least 30 days old (10% growth).

The summary names the measured commit and, for each regression, the baseline's
date and commit. Download those runs' `nightly-size-history` artifacts for
`sizes.json` and JS metafiles before rebuilding: the release sizes are actual
uploaded archives, and a local build on another platform is not a replacement.

Nightly passes its own run, so the sizes are the shipping artifacts for every
release target, Linux `x86_64` and `aarch64` included, without rebuilding them. Any
retained run works locally too. Each release job's `Build` step durations are
recorded and summarized but never compared, since hosted build time is noisy.
The Go mirror's 100 MiB file limit is enforced by the Go FFI package step;
the report tracks those staticlibs.

Keys name the artifact (which carries the target) and the filename without its
release version, so a version bump keeps the history comparable. Duplicate keys
and missing release artifacts are refused. A new artifact starts its own series,
named in the summary and compared from its next report, while the existing
series keep their history. A retained artifact that goes missing, including by
rename, is refused: review the change, then start a fresh history by reporting
without one.

A threshold failure **does write history** before returning failure. Nightly
uploads that history even when the check fails, then the next run uses it as
its daily baseline. An intended jump therefore fails once against the previous
point, rather than every night against the last passing point. The thirty-day
anchor still applies; advancing the daily baseline does not waive slow growth.
To accept measured growth, record the causing commits, before/after byte counts,
and reason in the implementation PR, and let the next scheduled run compare
against the retained point. Keep the thresholds unchanged. Confirm the next
run's measured revision and baseline before declaring the regression resolved.

A report refused for invalid input or missing artifacts writes no history, so
Nightly keeps loading the retained one. Only a reviewed restart of **all**
artifact series calls for a fresh history. Reporting locally without `--history`
creates one; Nightly's existing full reset deletes every retained history
artifact (including diagnostic metafiles), and does not belong in routine growth
acceptance:

```sh
gh api --paginate 'repos/{owner}/{repo}/actions/artifacts?name=nightly-size-history' \
    --jq '.artifacts[].id' | xargs -I{} gh api -X DELETE 'repos/{owner}/{repo}/actions/artifacts/{}'
```

A regression fails Nightly and reaches its existing alert workflow. The job
summary carries the byte counts, build times and FFI crate attribution, the
latter from the symbol-preserving `profiling` profile. Each night's byte counts
also join the [benchmark table](https://git.moq.dev/bench/), whose history
outlives the 45-day artifacts.
