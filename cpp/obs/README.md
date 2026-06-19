# obs-moq

An OBS Studio plugin for publishing to and subscribing from MoQ relays.

It loads into a stock OBS Studio install (no OBS source build required) and links
`libmoq`, built from the in-tree [`rs/libmoq`](../../rs/libmoq) crate.

Build instructions for each platform live in [`doc/bin/obs.md`](../../doc/bin/obs.md).
In short, from the repo root:

```bash
# Linux: the dev shell provides libobs/Qt6/ffmpeg
nix develop
just obs build

# macOS / Windows: needs Xcode / Visual Studio 2022; obs-deps download via buildspec.json
just obs setup
just obs build
```

Licensed under GPL-2.0-or-later (see [LICENSE](LICENSE)), separate from the rest of the
repository, because it links OBS.
