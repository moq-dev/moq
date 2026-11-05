/**
 * Content signing and end-to-end encryption for MoQ.
 *
 * {@link Sign} signs every grouped frame so a relay can proxy and cache a broadcast without
 * forging it. {@link E2ee} encrypts groups, datagrams, and track names so a relay forwards
 * ciphertext it cannot read. Mirrors the Rust `moq-crypto` crate's `sign` and `e2ee` modules.
 *
 * @module
 */

/** End-to-end encryption of groups, datagrams, and track names. */
export * as E2ee from "./e2ee/index.ts";
/** Per-frame Ed25519 signatures. */
export * as Sign from "./sign/index.ts";
