# [XS] A C++23 consumer links the package

## Goal

A consumer that raises the standard only on its own target
(`target_compile_features(app PRIVATE cxx_std_23)`) either links against the
`moq-cpp` package or is told clearly how to configure it. Today it gets link
errors: that requirement does not flow backward into the separate `moq-cpp`
static library, which compiles `moq.cpp` at `cxx_std_17` and so picks
`tl::expected` while the app's `<moq/moq.hpp>` picks `std::expected`. The ABI
guard turning that into a link error is correct; the surprise is not. Codex
found it on [#4187](https://github.com/moq-dev/moq/pull/4187).

## Plan

- Fix the misleading comment in `cpp/moq/cmake/moq-cpp-config.cmake.in`
  (and the matching one in `cpp/moq/CMakeLists.txt`): the bindings compile
  with the project's standard (`CMAKE_CXX_STANDARD`) or C++17, not with
  whatever the including target asks for.
- Then choose the smallest fix that makes the C++23 path work: document
  `CMAKE_CXX_STANDARD` (what the probe does) in `cpp/moq/README.md` and the
  C++ docs page, or add a package option that sets the standard `moq-cpp`
  compiles with. Prefer documentation unless a consumer (OBS, Unreal) needs
  the option.
- Test the documented configuration: the package test already builds at
  several standards; add one that builds the app at C++23 the documented
  way.

Public API: none, or one CMake option. Wire: none.
