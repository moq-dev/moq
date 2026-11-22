# [S] VA-API H.264 holds the bitrate on Intel iHD when a still scene turns busy

## Goal

`moq-video`'s `a_still_scene_turning_busy_holds_the_bitrate` passes on Intel
iHD as it does on radeonsi: an H.264 stream whose still picture turns to noise
keeps its worst second within the test's 2.6 Mbit/s bound at a 2 Mbit/s target.

## Plan

Found by [moq-dev/moq#9367](https://github.com/moq-dev/moq/pull/9367): on
Arrow Lake (iHD 26.3.5 from nixpkgs, `desktop`) the test fails on `main` at
d80a46c98a, with `Spent { mean: 1859904, peak: 2768656 }` for H.264. H.265
holds. The test's comment records only radeonsi peaks. No runner has an Intel
GPU, so neither CI nor `just rs gpu` runs it there.

Find why iHD overspends the first busy second, such as the HRD buffer fullness
or the window the driver applies, and fix the encoder's rate-control setup so it
holds. Widen the bound only if a measurement shows iHD cannot hold it under any
setting the encoder could send, and record that measurement in the test's
comment.

Run it with the flake's libva and `intel-media-driver`, as
`sh/rs/gpu.sh` does for the ladder test:
`LD_LIBRARY_PATH=<libva>/lib LIBVA_DRIVERS_PATH=<intel-media-driver>/lib/dri
cargo test -p moq-video --features vaapi,openh264 a_still_scene_turning_busy`.

Public API: none. Wire: none.

## Hosts

- linux - needs an Intel GPU with iHD, such as `desktop`'s Arrow Lake
