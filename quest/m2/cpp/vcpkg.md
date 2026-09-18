# [S] vcpkg registry for the prebuilt C++ package

## Goal

A consumer adds `moq-dev/vcpkg-registry` to `vcpkg-configuration.json`,
depends on `moq`, and gets the prebuilt package for their triple without a
Rust toolchain or the bindgen fork. A fresh consumer project installs it in
CI on Windows, macOS, and Linux.

## Plan

- `moq-dev/vcpkg-registry`: a git registry with a `moq` port whose portfile
  downloads the per-target release tarball from `release-cpp.yml` by version
  and hash, installs headers, the static library, and the CMake config, and
  declares `supports` for exactly the release matrix. Versioning follows the
  tarball tags.
- The port's version and hashes come from a release manifest checked into
  this repository, which `release-cpp.yml` updates and which opens the
  registry bump (like `release-brew.yml` does for Homebrew) once the tarballs
  are published. The Conan quest reads the same manifest.
- CI: a consumer smoke project (`vcpkg install` in manifest mode, then CMake)
  built nightly on all three platforms; a mismatch between the port and the
  tarball fails the nightly, not the user.
- Curated `microsoft/vcpkg` is out of scope; it wants source builds.

## Required

- [Package](/quest/m2/cpp/package.md) - the release tarballs the port fetches

## Related

- [Conan remote](/quest/m2/cpp/conan.md) - the same tarball through Conan
