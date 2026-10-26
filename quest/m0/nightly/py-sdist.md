# [XS] The Python sdist builds

## Goal

Nightly's Release Py FFI "Build sdist" job builds and installs the moq-ffi
sdist.

## Plan

Nightly [40192013303](https://github.com/moq-dev/moq/actions/runs/40192013303)
fails in maturin's `cargo metadata`: the sdist lacks
`rs/moq-ice/Cargo.toml`, which `moq-tokio` depends on since moq-ice was split
out. `pyproject.toml` lists the extra sdist paths by hand. Fix it so a new
workspace dependency cannot drop out of the sdist again, and keep the sdist
build in a lane that runs before release.

Public API: none. Wire: none.
