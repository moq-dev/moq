/**
 * JSON catalog schema and types describing a broadcast's tracks: audio/video decoder configs for
 * WebCodecs, container format, rendition layout, the `media` section (the broadcast's `clock` and
 * `join` interval),
 * plus the `json` and `binary` data sections, optional `stats` and `echo` discovery, broadcasts
 * listed in `include` without opening them, and the `drm` systems protecting CMAF renditions.
 *
 * @module
 */

import type * as z from "zod/mini";
import * as audio from "./audio";
import * as video from "./video";

/** Audio quality ordering. */
export const Audio = { ranked: audio.ranked };
/** The catalog audio section. */
export type Audio = z.infer<typeof audio.AudioSchema>;
/** Video quality ordering. */
export const Video = { ranked: video.ranked };
/** The catalog video section. */
export type Video = z.infer<typeof video.VideoSchema>;

export type { AudioConfig } from "./audio";
export { AudioConfigSchema, AudioSchema } from "./audio";
export * from "./binary";
export * from "./clock";
export * from "./container";
export * from "./drm";
export * from "./echo";
export * from "./format";
export * from "./hex";
export * from "./include";
export * from "./integers";
export * from "./json";
export * from "./media";
export * from "./mode";
export * from "./path";
export * from "./priority";
export * from "./root";
export * from "./stats";
export * from "./text";
export * from "./track";
export type { Color, Projection, VideoConfig } from "./video";
export { ColorSchema, ProjectionSchema, VideoConfigSchema, VideoSchema } from "./video";
