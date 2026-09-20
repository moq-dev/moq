# [S] Fallible NVENC loading

## Goal

Missing libraries, missing entry points, and unsupported NVIDIA driver versions
return errors through codec selection instead of aborting a process.

## Plan

`safe/api.rs` initializes the public lazy `ENCODE_API` by panicking. The
moq-video probe only establishes library presence, so an installed but
incompatible driver can still reach a version assertion. Mirror the fallible
loading contract of `cuvid::Api::get`; keep function-table initialization
private to the safe facade, and make public errors extensible.

Test absent libraries, absent symbols, an old API version, and successful
initialization with an injected loader in normal CI. Automatic selection must
fall through to another eligible backend; an explicitly named NVENC request
must return the reason it cannot open. Do not catch a panic as the design.

Public API: fallible loader access and extensible error variants in moq-nvenc.
Wire: none. Update its runtime-loading documentation and moq-video errors.
