# moq_ffi

Raw Dart and Flutter bindings for the MoQ protocol stack. Most applications
should use the higher-level [`moq`](https://pub.dev/packages/moq) package.

Native libraries are supplied through Dart Native Assets. Published releases
download a checksum-verified library for the target platform. A checkout of the
MoQ monorepo builds `rs/moq-ffi` locally instead.

The first release excludes the optional native audio and video codec features.
It provides the complete data-plane API, including sessions, announcements,
broadcasts, tracks, groups, frames, catalogs, and JSON tracks.
