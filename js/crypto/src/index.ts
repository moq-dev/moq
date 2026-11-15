/**
 * End-to-end encryption for MoQ.
 *
 * {@link E2ee} encrypts groups, datagrams, and track names so a relay forwards ciphertext it
 * cannot read. Mirrors the Rust `moq-crypto` crate's `e2ee` module.
 *
 * @module
 */

/** End-to-end encryption of groups, datagrams, and track names. */
export * as E2ee from "./e2ee/index.ts";
