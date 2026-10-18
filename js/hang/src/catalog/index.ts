/**
 * JSON catalog schema and types describing a broadcast's tracks: audio/video decoder configs for
 * WebCodecs, container format, and rendition layout, the `archive` entry and root `clock`,
 * plus the `json` and `binary` data sections and optional `stats` and `echo` discovery.
 *
 * @module
 */

import * as audio from "./audio";
import * as video from "./video";

/** Audio schemas and quality ordering. */
export const Audio = audio;
/** The catalog audio section. */
export type Audio = audio.Audio;
/** Video schemas and quality ordering. */
export const Video = video;
/** The catalog video section. */
export type Video = video.Video;

export * from "./archive";
export type { AudioConfig } from "./audio";
export { AudioConfigSchema, AudioSchema } from "./audio";
export * from "./binary";
export * from "./clock";
export * from "./compression";
export * from "./container";
export * from "./echo";
export * from "./format";
export * from "./hex";
export * from "./integers";
export * from "./json";
export * from "./mode";
export * from "./path";
export * from "./priority";
export * from "./root";
export * from "./stats";
export * from "./text";
export * from "./track";
export type { Color, VideoConfig } from "./video";
export { ColorSchema, VideoConfigSchema, VideoSchema } from "./video";
