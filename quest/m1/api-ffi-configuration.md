# [L] Make FFI configuration changes apply or fail explicitly

## Goal

Client, server, and pending-request configuration never silently ignores a
call because an async operation owns the state or the handle is closed.

## Plan

At dev `e2350b39a`, client setters in `rs/moq-ffi/src/session.rs:399-518`,
server setters in `server.rs:78-129`, and request origin setters at `:242`
use `if let Some(state) = self.task.lock()` and otherwise succeed without
changing anything. Even `set_bind` returns `Ok(())`. `ffi.rs:126` documents
that the lock returns `None` for both busy and cancelled state. A pending
connect/accept therefore makes configuration depend on scheduling.

Decided: configuration records captured at
construction/connect/listen/accept, with the lifecycle boundary visible in
the API.
Settle snapshot timing, handle reuse, and pending-request origins before
implementation. Do not preserve silent no-op setters as compatibility shims.

Reproduce mutation during a pending operation and after cancellation, then
test the chosen apply-or-error contract for each object family. Update every
handwritten wrapper and relevant C ABI entry, generated bindings through their
normal flow, examples, and language docs in the same change. Existing server
`cert_fingerprints` distinguishes busy and cancelled state and provides a
reference for the smaller design.

Public API: breaking configuration lifecycle or setter results. Wire: none.
Run `just check`, `just test`, and `just test smoke-full`.
