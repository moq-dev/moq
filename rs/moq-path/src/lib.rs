//! Path patterns: the one wildcard grammar every predicate over a MoQ broadcast path uses.
//!
//! A broadcast path is `/`-separated segments. A [`Pattern`] describes a set of them:
//!
//! - a literal segment matches itself, byte for byte;
//! - `*` matches exactly one segment, whatever it is;
//! - `**` matches any run of zero or more segments, and appears at most once.
//!
//! Wildcards are whole segments. `foo/*` matches `foo/bar` but not `foo/bar/baz` or `foo`,
//! `foo/**` matches all three, `**` matches every path, and the empty pattern matches
//! only the empty path. A `*` inside a literal (`*.hang`) is rejected rather than read
//! literally, which keeps that syntax free for a later, richer grammar.
//!
//! Patterns are exact: `foo` matches only `foo`. Write `foo/**` for a subtree. That is
//! the difference from a path prefix, which this crate never interprets.
//!
//! Beyond matching, [`Pattern`] has the algebra authorization and routing need:
//! [`contains`](Pattern::contains) decides whether one pattern's paths are all inside
//! another's (a grant covering a request), [`overlaps`](Pattern::overlaps) whether two
//! share any path, [`specificity`](Pattern::specificity) ranks the patterns matching one
//! path, and [`rebase`](Pattern::rebase) rewrites a pattern relative to a literal root,
//! which is set-valued because `**` may or may not have consumed the root. [`Patterns`]
//! is a union of patterns reduced by containment.
//!
//! Every operation is linear in the pattern and path lengths, and both are capped at
//! [`Pattern::MAX_SEGMENTS`]. The crate has no dependencies; enable the `serde` feature
//! to (de)serialize a pattern as its text.
//!
//! ```
//! use moq_path::Pattern;
//!
//! let transcode: Pattern = "**/transcode.pro".parse().unwrap();
//! assert!(transcode.matches("pid/foo.hang/transcode.pro"));
//! assert!(!transcode.matches("pid/foo.hang"));
//!
//! let grant: Pattern = "pid/**".parse().unwrap();
//! assert!(grant.contains(&"pid/*/chat".parse().unwrap()));
//! assert!(!grant.contains(&transcode));
//! ```

mod pattern;
mod patterns;

#[cfg(feature = "serde")]
mod serde;

pub use pattern::{Error, Pattern, Segment, Specificity};
pub use patterns::Patterns;
