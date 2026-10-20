# [S] Strip moq_ from generated C++ free functions

## Goal

Generated C++ free functions drop moq-ffi's `moq_` prefix the way generated
types already drop `Moq` (`strip_type_prefix` in `cpp/moq/uniffi.toml`), so
`moq::log_level` is the generated function itself. `moq.hpp`
(`cpp/moq/include/moq/moq.hpp`) then deletes its five pass-through wrappers:
`log_level`, `mint_epoch`, `client_defaults`, `versions` and
`qlog_supported`.

## Plan

Decided with the maintainer on 2026-10-17: the goal as drafted, the follow-up
#5727 deferred.

The option belongs in the uniffi-bindgen-cpp fork, released and pinned the
way #5727 pinned `strip_type_prefix` (every place that PR moved the pin).
`epoch_time` stays in `moq.hpp`, since it converts milliseconds to a
`time_point`, and calls the stripped `epoch_time_ms`. Callers in `cpp/obs` and
`doc/lib/cpp` already use the short names. The C++ package is unreleased, so
no upgrade note. Public API: C++ only, `moq::moq_*` free functions are gone.

## Related

- [Hard-fork the binding generator](/quest/m2/uniffi-fork.md) - where the generator option will live once the fork moves
