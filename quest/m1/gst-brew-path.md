# [S] Homebrew and Nix install a working moq-gst

## Goal

`brew install moq-gst` installs the plugin and `gst-inspect-1.0 moq` finds
it. Today the formula installs `lib/libgstmoq.*`, but the release tarball
keeps the plugin under `lib/gstreamer-1.0/`, so the glob matches nothing and
the formula installs nothing without an error. The published tap formula
(0.4.12) matches the template.

The Nix `moq-gst` package (`moq-gst` in `nix/overlay.nix`) also carries the
encoders and decoders a moq-gst pipeline needs: `x264enc`
(gst-plugins-ugly), and `avenc_aac` and `avdec_h264` (gst-libav), on its
plugin path beside base, good, and bad. moq.pro's
[gst-install-upstream](https://github.com/moq-dev/moq.pro/blob/main/quest/m1/gst-install-upstream.md)
waits on this, so its install can drop its own GStreamer setup.

## Plan

- `.github/homebrew/Formula/moq-gst.rb.tmpl` installs from
  `lib/gstreamer-1.0/`, the layout `rs/moq-gst/build.sh` packages and
  `.github/workflows/moq-gst.yml` smoke-tests. Its caveats and `test do`
  block move to the same path.
- Check on macOS whether Homebrew's `gstreamer` already scans
  `$(brew --prefix)/lib/gstreamer-1.0`. If it does, install there and drop the
  `GST_PLUGIN_PATH` caveat.
- [moq#4555](https://github.com/moq-dev/moq/pull/4555) carried this fix with a
  Nix wrapper change and closed unmerged on 2026-09-29. Nothing has owned it
  since.
- Nix: add gst-plugins-ugly and gst-libav to the `moq-gst` plugin path.
  Check `nix shell .#moq-gst --command gst-inspect-1.0` finds `x264enc`,
  `avenc_aac`, and `avdec_h264`, and add that check to CI.
- Regression check: the moq-gst workflow's macOS job installs the rendered
  formula against the tarball it just built and runs `brew test moq-gst`, so a
  formula and tarball that disagree fail before release.

Public API: none. Wire: none.
