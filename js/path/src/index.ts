/**
 * Path patterns: the one wildcard grammar every predicate over a MoQ broadcast path uses.
 *
 * A broadcast path is `/`-separated segments. A {@link Pattern} describes a set of them:
 * a literal segment matches itself, `*` matches exactly one segment, and `**` matches any
 * run of zero or more segments, at most once per pattern. Wildcards are whole segments,
 * and patterns are exact (`foo` matches only `foo`; write `foo/**` for a subtree).
 *
 * {@link Patterns} is a union of patterns reduced by containment. Both mirror the Rust
 * `moq-path` crate, and the two are held to the same test vectors.
 *
 * @module
 */

export * from "./pattern.ts";
