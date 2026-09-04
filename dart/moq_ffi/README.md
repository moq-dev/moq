# moq\_ffi

Raw Dart and Flutter bindings for the MoQ protocol stack. Most applications
should use the higher-level [`moq`](https://pub.dev/packages/moq) package.

Native libraries are supplied through Dart Native Assets. Published releases
download a checksum-verified library for the target platform. A checkout of the
MoQ monorepo builds `rs/moq-ffi` locally instead.

To supply your own build, point the hook at it from your application's
`pubspec.yaml`:

```yaml
hooks:
  user_defines:
    moq_ffi:
      library: path/to/libmoq_ffi.dylib
```

The path is resolved relative to the `pubspec.yaml` that declares it. This is
the only override: build hooks run with a filtered environment, so an
environment variable cannot reach one.

The first release excludes the optional native audio and video codec features.
It provides the complete data-plane API, including sessions, announcements,
broadcasts, tracks, groups, frames, catalogs, and JSON tracks.
