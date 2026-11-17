/**
 * JSON catalog schema and types describing a broadcast's tracks: audio/video decoder configs for
 * WebCodecs, container format, rendition layout, the `sync` section (the broadcast's `clock` and
 * join `interval`),
 * plus the `json` and `binary` data sections, optional `stats` and `echo` discovery, and
 * broadcasts listed in `include` without opening them.
 *
 * @module
 */

import type * as z from "zod/mini";
import * as audio from "./audio.ts";
import * as video from "./video.ts";

/** Audio quality ordering. */
export const Audio = { ranked: audio.ranked };
/** The catalog audio section. */
export type Audio = z.infer<typeof audio.AudioSchema>;
/** Video quality ordering. */
export const Video = { ranked: video.ranked };
/** The catalog video section. */
export type Video = z.infer<typeof video.VideoSchema>;

export type { AudioConfig } from "./audio.ts";
export { AudioConfigSchema, AudioSchema } from "./audio.ts";
export * from "./binary.ts";
export * from "./clock.ts";
export * from "./container.ts";
export * from "./echo.ts";
export * from "./format.ts";
export * from "./hex.ts";
export * from "./include.ts";
export * from "./integers.ts";
export * from "./json.ts";
export * from "./mode.ts";
export * from "./path.ts";
export * from "./priority.ts";
export * from "./root.ts";
export * from "./stats.ts";
export * from "./text.ts";
export * from "./timeline.ts";
export * from "./track.ts";
export type { Color, Projection, VideoConfig } from "./video.ts";
export { ColorSchema, ProjectionSchema, VideoConfigSchema, VideoSchema } from "./video.ts";
