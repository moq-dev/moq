import { z } from "zod";

/**
 * Container format for frame timestamp encoding.
 *
 * - "legacy": Uses QUIC VarInt encoding (1-8 bytes, variable length)
 * - "raw": Uses fixed u64 encoding (8 bytes, big-endian)
 * - "fmp4": Fragmented MP4 container (future)
 */
export const ContainerSchema = z.enum(["legacy", "raw", "fmp4"]);

export type Container = z.infer<typeof ContainerSchema>;

/**
 * Default container format when not specified.
 * Set to legacy for backward compatibility.
 */
export const DEFAULT_CONTAINER: Container = "legacy";
