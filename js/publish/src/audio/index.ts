import type * as Catalog from "@moq/hang/catalog";
import type { Rendition as BaseRendition } from "../rendition";

export * from "./capture";
export {
	type Aac,
	type AacConfig,
	type Codec,
	Encoder,
	type EncoderInput,
	type EncoderOutput,
	type EncoderProps,
	type Opus,
	type OpusConfig,
	type Stats,
} from "./encoder";
export type {
	CodecMime,
	Constraints,
	Kind,
	SampleSource,
	Source,
	SourceConfig,
	StreamTrack,
	TrackSettings,
} from "./types";

/** A registered audio rendition on a Broadcast. See {@link BaseRendition}. */
export type Rendition = BaseRendition<Catalog.AudioConfig>;
