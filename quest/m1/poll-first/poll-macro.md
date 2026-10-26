# [M] A kio macro for poll bodies

## Goal

A spike report, with a prototype in kio, on whether a proc macro should write
`poll_*` state machines: from straight-line code with explicit wait points, it
generates a named struct holding the values that live across each wait, and a
`poll(&mut self, &Waiter) -> Poll<T>` that resumes where it left off. The
prototype converts one real moq-net `poll_*` with several wait points. The report
compares it with the hand-written version on compile time (clean and edit), code
size (`cargo llvm-lines`), readability, error spans, and cancellation, and
recommends adopting it or not.

## Plan

Decided with the maintainer on 2026-10-25: spike it in kio. Hand-written `poll_*`
stays the rule until the report says otherwise.

- **What it must keep from kio:**
  - no `Pin`: the state is a named, `Unpin` struct;
  - progress in named fields, so dropping mid-wait leaves an inspectable state,
    not a silently abandoned future;
  - `Send` follows from the struct's fields, which a caller can name and bound.
- **What it must avoid from async:**
  - an anonymous type whose layout leaks through rmeta;
  - compile time that grows with nesting;
  - hidden yield points.
- **Prior art:** genawaiter, the coroutine lowering in rustc, `async-stream`, and
  hand-rolled `poll_fn` patterns. Say what each gets wrong for kio.
- **Expansion cost:** the macro's expansion counts against the compile time it
  saves, so measure clean and edit builds of the converted module.

Public API: none unless adopted; then kio gains the macro.
