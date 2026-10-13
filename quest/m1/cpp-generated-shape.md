# [S] One expected type and unprefixed names in the generated C++

## Goal

Before the first `cpp-v*` release, `cpp/moq` gives each type one name and
`moq::expected` one type. `moq::Client` is the generated type itself, not an
alias of `moq::MoqClient`. `moq::expected` is the bundled `tl::expected` at
every C++ standard, so a consumer built at C++23 links against bindings built
at C++17.

## Plan

Decided while iterating on #4079: both changes are breaking once a release
ships, and both need the generator fork
([kixelated/uniffi-bindgen-cpp](https://github.com/kixelated/uniffi-bindgen-cpp)),
so they share one fork release and one pin bump. `flake.nix` lists every
place that names the pin.

- **Names**: the fork gains a rename option that strips a type prefix, and
  `cpp/moq/uniffi.toml` strips `Moq`. Delete the alias list in
  `cpp/moq/include/moq/moq.hpp` and the `just cpp check` audit of it, and
  drop the "each generated `moq::MoqFoo` is also `moq::Foo`" lines from
  `doc/lib/cpp/index.md` and `cpp/moq/README.md`. Then update the C++ note
  in [The bindings mirror Rust's layers](/quest/m1/ffi-shape/README.md),
  which keeps C++ flat with `Media*` names until this lands.
- **expected**: the fork gains an option that always uses the bundled
  `tl::expected`. Delete the `MOQ_ABI` guard in `moq.hpp` and
  `src/moq.cpp`, the "C++ standard" sections of `doc/lib/cpp/index.md` and
  `cpp/moq/README.md`, and the `test-target-23` build in `sh/cpp/check.sh`,
  which only exercises the guard. Keep its C++17 and C++23 probe builds,
  which prove the bindings compile at both. C++23 users lose
  `std::expected`, which costs little: Unreal is C++20, and `tl::expected`
  has the same members. With exceptions off, the bundled `tl::expected`
  makes `value()` on an error `__builtin_unreachable`; patch it to abort
  with a message, like a misused future, and drop the "undefined" warning
  from the Errors section of `doc/lib/cpp/index.md`.
