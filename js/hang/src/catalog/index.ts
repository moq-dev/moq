/**
 * JSON catalog schema and types describing a broadcast's tracks: audio/video decoder configs for
 * WebCodecs, container format, rendition layout, the `media` section (the broadcast's `clock` and
 * `join` interval),
 * plus the `json` and `binary` data sections, optional `stats` and `echo` discovery, whole
 * `contributions` listed without opening them, and the `drm` systems protecting CMAF renditions.
 *
 * @module
 */

import * as audio from "./audio";
import * as video from "./video";

/** Audio schemas and quality ordering. */
export { audio as Audio };
/** The catalog audio section. */
export type Audio = audio.Audio;
/** Video schemas and quality ordering. */
export { video as Video };
/** The catalog video section. */
export type Video = video.Video;

export type { AudioConfig } from "./audio";
export { AudioConfigSchema, AudioSchema } from "./audio";
export * from "./binary";
export * from "./clock";
export * from "./container";
export * from "./contribution";
export * from "./drm";
export * from "./echo";
export * from "./format";
export * from "./hex";
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
