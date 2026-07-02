import { Path } from "@moq/net";
import * as z from "zod/mini";

/**
 * Zod schema for a relative broadcast reference stored in a catalog (a rendition's
 * `broadcast` field, e.g. "../source"). Normalizes the input the same way the Rust
 * `PathRelative` type does so JS and Rust agree byte-for-byte after deserialization.
 * Resolve it against the catalog broadcast's own path with `Path.resolve`.
 */
export const RelativeBroadcastSchema = z.pipe(z.string(), z.transform(Path.normalizeRelative));

/** A normalized relative broadcast reference. */
export type RelativeBroadcast = z.infer<typeof RelativeBroadcastSchema>;
