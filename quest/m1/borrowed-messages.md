# [L] Messages borrow instead of owning

## Goal

moq-net's message structs borrow their variable-length fields instead of owning them:

- **Decode:** a message holds `&'a str` and `&'a [u8]` sliced from the wire buffer, so
  a path that is only looked up never allocates.
- **Encode:** a message holds `&'a Path` and other fields borrowed from the caller, so
  encoding never clones them.

Only code that stores a field converts it, as with `Path::from`. Allocation-counting
tests show fewer allocations per decoded and encoded message than before, and
none more on any path.

## Plan

Decided with the maintainer on 2026-10-25:

- **Path stays owned and lifetime-free.** The lifetime-free `Path` change lands first,
  and messages hold an owned `Path` until this quest. Borrowing lives in the message
  structs, not in `Path`: most decoded paths are stored and allocate anyway; route
  lookup walks `&str` segments; and inline storage makes paths up to 22 bytes free.
- **m1, before the cut,** because message types are public moq-net API. The break rides
  the same release as lifetime-free `Path`, with one MIGRATE entry per affected crate.
- **Codec traits:** `Decode` gains the buffer's lifetime
  (`fn decode(r: &mut Decoder<'a>, ..) -> Result<Self>` for `Self: 'a`), so a message
  can borrow from the wire. moq-dev/moq#6826's `Decoder::str` is the starting point.
- **Where it pays:**
  - a subscribe to an unknown broadcast or an unroutable request, which costs 3 to
    5 allocations today;
  - lite-07's compressed path `rest`, which is allocated before it is joined;
  - IETF namespaces, which are 2 allocations today.
- Reuse `lite::PathRef<'a>` where it already fits, rather than adding a borrowed path
  type.

Public API: every message type with a variable-length field gains a lifetime, and
`Decode` gains one.
