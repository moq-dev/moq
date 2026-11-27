# [M] Typed superseded group end in moq-net

## Goal

A group cut short because a newer one superseded it, such as one an on-demand track's dropped producer left open while a reader joined, reaches readers as a typed, skippable group end from `moq-net`'s group API, the same over moq-lite and moq-transport, and libmoq's C raw reader (`rs/libmoq/src/consume.rs`, `run_group`) and moq-ffi's ordered reader (`rs/moq-ffi/src/consumer.rs`) branch on it instead of matching `Error::Old` and `StreamError::Old`. Today over moq-transport that group arrives as a cancelled stream rather than `Old`, so the C raw reader ends the whole subscription instead of reading on to the next producer, and both readers match on error variants, which AGENTS.md forbids. A publisher's explicit group abort still ends the track with its error.
