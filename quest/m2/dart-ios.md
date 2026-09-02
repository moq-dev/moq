# [S] Dart on iOS

## Goal

An iOS application actually loads the published `moq_ffi` native asset and
completes a round trip. Today the build is correct but has never been run on a
device or simulator, and `/doc/lib/dart` already promises an iOS 16 floor.

## Plan

The bindings originally declared iOS assets with `StaticLinking()`, which
`code_assets` documents as unimplemented in the Dart and Flutter SDK
([dart-lang/sdk#49418](https://github.com/dart-lang/sdk/issues/49418)). No iOS
application could have consumed the `libmoq_ffi.a` the release matrix built,
while the docs advertised the platform. Fixed in
[#3215](https://github.com/moq-dev/moq/pull/3215) by shipping the cdylib
through `DynamicLoadingBundled`, the one link mode the SDK does implement.

What that fix rests on, and what is still only inference:

- `rustc` emits a real `PLATFORM_IOS` dylib for both `aarch64-apple-ios` and
  `-sim`, verified with `otool -l`. So the artifact exists.
- `rs/moq-ffi/build.sh` packages it next to the staticlib Swift links, and CI
  builds all three iOS targets green. So the artifact ships.
- Nobody has loaded it. CI cannot: there is no iOS runner, and native assets
  only resolve inside a real Flutter build.

Run a minimal Flutter application on a simulator and on a device, and confirm
`Moq.connect` reaches a relay. Then check the parts an embedded dynamic library
is subject to and a desktop one is not: that Flutter bundles it into
`Frameworks/`, that codesigning covers it, and that a TestFlight or App Store
upload is accepted. A rejection there is the outcome worth knowing early, and
would send this back to a static-linking story that waits on the SDK.

Record the verdict either way. If iOS cannot work, say so in `/doc/lib/dart`
and drop the three iOS targets from `release-dart-ffi.yml` rather than paying
~15 minutes a release for an asset nothing loads.

## Related

- [Dart publish](/quest/m2/dart-publish.md) - requires this verdict, since publishing spreads the iOS claim
