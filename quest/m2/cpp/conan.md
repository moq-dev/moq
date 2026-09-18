# [S] Conan remote for the prebuilt C++ package

## Goal

A consumer adds the moq Conan remote, requires `moq/<version>`, and gets the
prebuilt package for their `os`, `arch`, and `compiler` without a Rust
toolchain or the bindgen fork. A fresh consumer project installs it in CI on
Windows, macOS, and Linux.

## Plan

- A `moq` recipe on a moq-dev remote (Artifactory or a GitHub-hosted `conan`
  index) that packages the prebuilt release tarball per setting and exports
  the CMake target from `package_info`.
- The recipe reads the release manifest the vcpkg quest introduced, so one
  release bumps both recipes; `release-cpp.yml` publishes to the remote after
  the tarballs land.
- CI: a consumer smoke project (`conan install` then CMake) built nightly on
  all three platforms.
- conan-center is out of scope; it wants source builds.

## Required

- [vcpkg registry](/quest/m2/cpp/vcpkg.md) - the release manifest and bump automation this reuses
