# Nightly artifact sizes

`just gh size-collect OUTPUT [RUN]` measures what a workflow run's release jobs
uploaded, plus the builds no release job makes: the minimal-feature FFI library
and real browser consumer bundles (raw, gzip and Brotli). `just gh size-report
OUTPUT [--history HISTORY]` compares the report with its previous report (5%
growth) and the newest report at least 30 days old (10% growth).

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
without one. A refused report writes no history, so Nightly keeps loading the
retained one; reset it by deleting every `nightly-size-history` artifact:

```sh
gh api --paginate 'repos/{owner}/{repo}/actions/artifacts?name=nightly-size-history' \
    --jq '.artifacts[].id' | xargs -I{} gh api -X DELETE 'repos/{owner}/{repo}/actions/artifacts/{}'
```

A regression fails Nightly and reaches its existing alert workflow. The job
summary carries the byte counts, build times and FFI crate attribution, the
latter from the symbol-preserving `profiling` profile.
