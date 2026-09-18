# [M] vcpkg registry and Conan remote for the prebuilt C++ package

## Goal

A consumer adds `moq-dev/vcpkg-registry` to `vcpkg-configuration.json` and
depends on `moq`, or adds the moq Conan remote and requires `moq/<version>`,
and gets the prebuilt package for their triple without a Rust toolchain or the
bindgen fork. Both ports are exercised in CI against a fresh consumer project
on Windows, macOS, and Linux.

## Plan

- `moq-dev/vcpkg-registry`: a git registry with a `moq` port whose portfile
  downloads the per-target release tarball from `release-cpp.yml` by version
  and hash, installs headers, the static library, and the CMake config, and
  declares `supports` for exactly the release matrix. Versioning follows the
  tarball tags.
- Conan: a `moq` recipe on a moq-dev remote (Artifactory or a GitHub-hosted
  `conan` index) that packages the same prebuilt tarball per `settings.os`,
  `arch`, and `compiler`, with `package_info` exporting the CMake target.
- vcpkg first, Conan in the same PR series; both recipes are generated from
  one manifest in this repository so a release bumps them together.
  `release-cpp.yml` opens the registry bump (like `release-brew.yml` does for
  Homebrew) once the tarballs are published.
- CI: a consumer smoke project per registry (`vcpkg install` in manifest mode,
  `conan install` + CMake) built nightly on all three platforms; a mismatch
  between the port and the tarball fails the nightly, not the user.
- Curated `microsoft/vcpkg` and conan-center submissions are out of scope;
  they want source builds. Revisit when someone asks.

## Required

- [Package](/quest/m2/cpp/package.md) - the release tarballs the ports fetch
