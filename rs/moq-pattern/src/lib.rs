//! Path patterns and prefix grants for Media over QUIC.
//!
//! A [`Pattern`] describes a set of broadcast paths, for discovery filters that pick which
//! announcements a reader looks at. [`Patterns`] is an unordered union reduced by exact
//! containment. Matching is linear. [`Pattern::literal`] rejects `*` because it is
//! reserved for pattern syntax.
//!
//! A [`Prefix`] is a grant member: a path covering itself and every path beneath it, the
//! empty prefix covering everything. [`Prefixes`] is a grant, a reduced set of them. A
//! grant takes no wildcard: parsing one fails with [`InvalidPrefix::Wildcard`].
//!
//! `moq-net` and `moq-auth` re-export this crate. The TypeScript twin is `@moq/pattern`.
//!
//! # Grammar
//!
//! A pattern is canonical `/`-separated segments:
//!
//! - a literal;
//! - `*`, matching one complete segment;
//! - `lit*lit`, with one `*` matching bytes inside one segment;
//! - `**`, matching zero or more complete segments, at most once per pattern.
//!
//! Segments are split as [`parts`] splits a path: a `\` escapes the `\` or `/` after it,
//! so `a\/b` is one literal segment holding a `/`, and any other `\` is refused. A literal
//! matches the segment spelled the same way, which is how a grant names a moq-transport
//! namespace field holding a `/`. There is no escape for `*`.
//!
//! Patterns are exact: `foo` matches only `foo`, `foo/**` matches its subtree including
//! `foo`, `**` matches every path, and the empty pattern matches only the current root.
//! Parse rejects leading, trailing, or repeated `/`, more than one `*` in a segment,
//! `**` mixed with literal bytes, more than one `**`, and more than [`Pattern::MAX_SEGMENTS`]
//! (32) segments. Construction moves `**` before adjacent `*` segments, so `*/**` is
//! `**/*`.
//!
//! # Algebra
//!
//! [`Pattern::matches`], [`Pattern::overlaps`], [`Pattern::contains`], [`Pattern::head`],
//! [`Pattern::specificity`], set-valued [`Pattern::rebase`] and [`Pattern::intersect`],
//! and [`Pattern::captures`]. A rebase never picks one lossy residual: `**/a` at `a` is
//! both the empty pattern and `**/a`, and an intersection never picks one lossy
//! overlap: `a/**` with `**/a` is both `a` and `a/**/a`. A union reduces per member; a
//! candidate covered only jointly by several members is refused.
//!
//! # CAT / C4M
//!
//! [Common Access Token](https://shop.cta.tech/products/cta-5007) and
//! [`draft-ietf-moq-c4m-01`](https://datatracker.ietf.org/doc/draft-ietf-moq-c4m/) match
//! namespace fields positionally: exact, prefix, or suffix per field, with a trailing
//! `nil` for exact depth. Without `nil`, longer namespaces that start with the matching
//! fields are in scope.
//! A pattern segment is one field, compared by its [`unescape`]d content.
//!
//! That common subset is:
//!
//! | Pattern | C4M |
//! | --- | --- |
//! | `foo/bar` | exact `foo`, exact `bar`, `nil` |
//! | `foo/bar/**` | exact `foo`, exact `bar` (no `nil`) |
//! | `foo*` | prefix `foo` on that field |
//! | `*foo` | suffix `foo` on that field |
//! | `*` | prefix of the empty byte string (any field) |
//! | `pid/*/chat` | exact `pid`, any field, exact `chat`, `nil` |
//!
//! Richer MoQ forms, kept explicit rather than claimed as CAT gaps:
//!
//! - `**` not at the end (`**/a`, `a/**/b`): C4M is positional from the front.
//! - `foo*bar`: C4M's match object is exact, prefix, *or* suffix, not both.
//!
//! # Literal paths
//!
//! [`Path`](https://docs.rs/moq-net/latest/moq_net/struct.Path.html) stays a coordinate.
//! Roots, joins, exact names, URL paths, and object-store keys keep their own types.
//! [`Pattern::literal`] rejects `*`; literal `Path` construction and wire decoding
//! retain their existing behavior.

#![warn(missing_docs)]

mod index;
mod parts;
mod pattern;
mod patterns;
mod prefix;

pub use parts::{Parts, escape, parts, unescape};
pub use pattern::{IntersectionError, InvalidPattern, Pattern, Segment, Specificity};
pub use patterns::Patterns;
pub use prefix::{InvalidPrefix, Prefix, Prefixes};
