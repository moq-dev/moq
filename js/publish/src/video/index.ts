import type * as Catalog from "@moq/hang/catalog";
import type { Rendition as BaseRendition } from "../rendition";

export * from "./capture";
export * from "./encoder";
export type { Constraints, FrameSource, Source, SourceConfig, StreamTrack, TrackSettings } from "./types";

/** A registered video rendition on a Broadcast. See {@link BaseRendition}. */
export type Rendition = BaseRendition<Catalog.VideoConfig>;
