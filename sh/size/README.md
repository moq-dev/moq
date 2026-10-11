# Nightly artifact sizes

`just gh size-collect OUTPUT [PACKAGED_ARTIFACTS]` builds native artifacts and
real browser consumer bundles. `just gh size-report OUTPUT [HISTORY]` records
raw, gzip and Brotli bytes and compares the report with its previous report
(5% growth) and the newest report at least 30 days old (10% growth).

The Nightly job reuses wheel, Kotlin Maven/AAR and Swift XCFramework packaging
outputs. It retains byte counts, bundle metafiles and native crate attribution,
without uploading another copy of native build products. A regression fails
Nightly and reaches its existing alert workflow. No direct notification is sent.

Native keys include the Rust host triple. Packaged keys retain distribution,
platform and feature identity while omitting release versions; Kotlin measures
both shipping JARs and AARs and excludes source and Javadoc archives. Duplicate
canonical keys are refused.

An artifact-set change is refused. Review the new set and start a fresh history
explicitly by invoking the reporting recipe without a history input; do not mix
platforms, feature sets or packaging formats in one history. Histories are
versioned, and a collector failure searches earlier retained runs for a baseline.

Land the uniform release-profile settings before accepting a shipping baseline.
The local reducer and bundle collector are verified independently; the complete
native and packaged matrix still needs a Nightly dry run before this quest ends.
