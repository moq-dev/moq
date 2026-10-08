# [S] Conan remote for the prebuilt C++ package

## Goal

A consumer adds the moq Conan remote, requires `moq-cpp/<version>`, and gets the
prebuilt package for their `os`, `arch`, and `compiler` without a Rust
toolchain or the bindgen fork. A fresh consumer project installs it in CI on
Windows, macOS, and Linux.

## Plan

In m3 until a Conan consumer asks; the release tarball covers C++ consumers
first. The vcpkg registry, which this reuses, is parked in m3 too (decided
2026-10-08), so it lands before this does.

- A `moq-cpp` recipe, named after the package, on a moq-dev remote
  (Artifactory or a GitHub-hosted `conan` index) that packages the prebuilt
  release tarball per setting and exports the `moq::cpp` CMake target from
  `package_info`.
- The recipe reads the release manifest the vcpkg quest introduced, so one
  release bumps both recipes; `release-cpp.yml` publishes to the remote after
  the tarballs land.
- CI: a consumer smoke project (`conan install` then CMake) built nightly on
  all three platforms.
- conan-center is out of scope; it wants source builds.
- Decided 2026-10-08: like the vcpkg port, this needs the first C++ package
  release, which #4079 adds as its own quest (`quest/m1/cpp-release.md`).
  Once that file is on `main`, it replaces the line README as Required here.

## Required

- [C++ through moq-ffi](/quest/m1/cpp/README.md) - the release tarballs the recipe packages
- [vcpkg registry](/quest/m3/cpp-vcpkg.md) - the release manifest and bump automation this reuses
