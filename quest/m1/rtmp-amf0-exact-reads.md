# [XS] AMF0 reads exactly what it declared

## Goal

A truncated AMF0 string or property name fails with `UnexpectedEof`, instead of succeeding with NUL padding.

Today `rs/moq-rtmp/src/rml/amf0/deserialization.rs` allocates the declared length and calls `bytes.read(&mut buffer)`, ignoring the returned count. `Read` for a byte slice returns a short read at EOF, so the zero fill is parsed as content. A publish stream key or object property can silently carry embedded NULs, and two RTMP implementations disagree about what the frame said.

## Plan

Decided 2026-10-22 from the security audit, with the maintainer:

- Use `read_exact` at both sites.
- Test both sites: the audit's string payload (string marker, declared length 16, one byte present), and an object whose property name declares more bytes than remain. Each must error rather than yield a padded value.

The RTMP listener is unauthenticated by design, so this is reachable by anyone who can reach the port.

Public API: none. Wire: none.
