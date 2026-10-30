# `@moq/hang/catalog`

```ts
import { $ZodBranded } from "zod/v4/core";
import { $ZodTypeInternals } from "zod/v4/core";
import { $brand } from "@zod/mini";
import { $loose } from "zod/v4/core";
import { $strip } from "zod/v4/core";
import { Net } from "@moq/hang";
import { ZodMiniBase64 } from "@zod/mini";
import { ZodMiniBoolean } from "@zod/mini";
import { ZodMiniCatch } from "@zod/mini";
import { ZodMiniCustom } from "@zod/mini";
import { ZodMiniDefault } from "@zod/mini";
import { ZodMiniDiscriminatedUnion } from "@zod/mini";
import { ZodMiniEnum } from "@zod/mini";
import { ZodMiniInt32 } from "@zod/mini";
import { ZodMiniLiteral } from "@zod/mini";
import { ZodMiniNumber } from "@zod/mini";
import { ZodMiniObject } from "@zod/mini";
import { ZodMiniOptional } from "@zod/mini";
import { ZodMiniPipe } from "@zod/mini";
import { ZodMiniRecord } from "@zod/mini";
import { ZodMiniString } from "@zod/mini";
import { ZodMiniTransform } from "@zod/mini";
import { ZodMiniType } from "@zod/mini";
import { ZodMiniUnion } from "@zod/mini";
import { infer } from "@zod/mini";

export namespace Audio {
	export type Audio = infer<typeof Audio.AudioSchema>;
	export type AudioConfig = infer<typeof Audio.AudioConfigSchema>;
	export const AudioConfigSchema: ZodMiniObject<{
		bitrate: ZodMiniOptional<$ZodBranded<ZodMiniNumber<number>, "u53", "out">>;
		broadcast: ZodMiniOptional<ZodMiniType<RelativeBroadcast, string, $ZodTypeInternals<RelativeBroadcast, string>>>;
		codec: ZodMiniString<string>;
		container: ZodMiniDefault<ZodMiniUnion<readonly [ZodMiniDiscriminatedUnion<[ZodMiniObject<{
			kind: ZodMiniLiteral<"legacy">;
		}, $strip>, ZodMiniObject<{
			init: ZodMiniBase64;
			kind: ZodMiniLiteral<"cmaf">;
		}, $strip>, ZodMiniObject<{
			kind: ZodMiniLiteral<"loc">;
		}, $strip>], "kind">, ZodMiniPipe<ZodMiniObject<{
			kind: ZodMiniString<string>;
		}, $loose>, ZodMiniTransform<{
			kind: "unknown";
			raw: {
				[x: string]: unknown;
				kind: string;
			};
		}, {
			[x: string]: unknown;
			kind: string;
		}>>]>>;
		delay: ZodMiniOptional<ZodMiniPipe<$ZodBranded<ZodMiniNumber<number>, "u53", "out">, ZodMiniTransform<number & $brand<"u53"> | undefined, number & $brand<"u53">>>>;
		description: ZodMiniOptional<ZodMiniString<string>>;
		enabled: ZodMiniOptional<ZodMiniBoolean<boolean>>;
		epoch: ZodMiniOptional<ZodMiniType<ReferenceEpoch, string, $ZodTypeInternals<ReferenceEpoch, string>>>;
		hls: ZodMiniCatch<ZodMiniOptional<ZodMiniObject<{
			duration: $ZodBranded<ZodMiniNumber<number>, "u53", "out">;
			track: ZodMiniString<string>;
		}, $strip>>>;
		jitter: ZodMiniOptional<ZodMiniPipe<$ZodBranded<ZodMiniNumber<number>, "u53", "out">, ZodMiniTransform<number & $brand<"u53"> | undefined, number & $brand<"u53">>>>;
		label: ZodMiniOptional<ZodMiniString<string>>;
		layout: ZodMiniOptional<ZodMiniString<string>>;
		numberOfChannels: $ZodBranded<ZodMiniNumber<number>, "u53", "out">;
		sampleRate: $ZodBranded<ZodMiniNumber<number>, "u53", "out">;
		track: ZodMiniOptional<ZodMiniString<string>>;
		warmup: ZodMiniOptional<$ZodBranded<ZodMiniNumber<number>, "u53", "out">>;
	}, $strip>;
	export const AudioSchema: ZodMiniObject<{
		renditions: ZodMiniPipe<ZodMiniCustom<Record<string, unknown>, Record<string, unknown>>, ZodMiniTransform<Record<string, {
			bitrate?: number & $brand<"u53"> | undefined;
			broadcast?: RelativeBroadcast | undefined;
			codec: string;
			container: {
				kind: "legacy";
			} | {
				init: string;
				kind: "cmaf";
			} | {
				kind: "loc";
			} | {
				kind: "unknown";
				raw: {
					[x: string]: unknown;
					kind: string;
				};
			};
			delay?: number & $brand<"u53"> | undefined;
			description?: string | undefined;
			enabled?: boolean | undefined;
			epoch?: ReferenceEpoch | undefined;
			hls?: {
				duration: number & $brand<"u53">;
				track: string;
			} | undefined;
			jitter?: number & $brand<"u53"> | undefined;
			label?: string | undefined;
			layout?: string | undefined;
			numberOfChannels: number & $brand<"u53">;
			sampleRate: number & $brand<"u53">;
			track?: string | undefined;
			warmup?: number & $brand<"u53"> | undefined;
		}>, Record<string, unknown>>>;
	}, $strip>;
	export function ranked(renditions: Record<string, Audio.AudioConfig>): [string, Audio.AudioConfig][];
}

export type Audio = Audio.Audio;

export import AudioConfig = Audio.AudioConfig;

export import AudioConfigSchema = Audio.AudioConfigSchema;

export import AudioSchema = Audio.AudioSchema;

export type Binary = infer<typeof BinarySchema>;

export type BinaryConfig = infer<typeof BinaryConfigSchema>;

export const BinaryConfigSchema: ZodMiniObject<{
	bitrate: ZodMiniOptional<$ZodBranded<ZodMiniNumber<number>, "u53", "out">>;
	broadcast: ZodMiniOptional<ZodMiniType<RelativeBroadcast, string, $ZodTypeInternals<RelativeBroadcast, string>>>;
	delay: ZodMiniOptional<ZodMiniPipe<$ZodBranded<ZodMiniNumber<number>, "u53", "out">, ZodMiniTransform<number & $brand<"u53"> | undefined, number & $brand<"u53">>>>;
	epoch: ZodMiniOptional<ZodMiniType<ReferenceEpoch, string, $ZodTypeInternals<ReferenceEpoch, string>>>;
	jitter: ZodMiniOptional<ZodMiniPipe<$ZodBranded<ZodMiniNumber<number>, "u53", "out">, ZodMiniTransform<number & $brand<"u53"> | undefined, number & $brand<"u53">>>>;
	mime: ZodMiniOptional<ZodMiniString<string>>;
	mode: ZodMiniString<string>;
	track: ZodMiniOptional<ZodMiniString<string>>;
}, $loose>;

export const BinarySchema: ZodMiniObject<{
	tracks: ZodMiniRecord<ZodMiniString<string>, ZodMiniObject<{
		bitrate: ZodMiniOptional<$ZodBranded<ZodMiniNumber<number>, "u53", "out">>;
		broadcast: ZodMiniOptional<ZodMiniType<RelativeBroadcast, string, $ZodTypeInternals<RelativeBroadcast, string>>>;
		delay: ZodMiniOptional<ZodMiniPipe<$ZodBranded<ZodMiniNumber<number>, "u53", "out">, ZodMiniTransform<number & $brand<"u53"> | undefined, number & $brand<"u53">>>>;
		epoch: ZodMiniOptional<ZodMiniType<ReferenceEpoch, string, $ZodTypeInternals<ReferenceEpoch, string>>>;
		jitter: ZodMiniOptional<ZodMiniPipe<$ZodBranded<ZodMiniNumber<number>, "u53", "out">, ZodMiniTransform<number & $brand<"u53"> | undefined, number & $brand<"u53">>>>;
		mime: ZodMiniOptional<ZodMiniString<string>>;
		mode: ZodMiniString<string>;
		track: ZodMiniOptional<ZodMiniString<string>>;
	}, $loose>>;
}, $strip>;

export type Clock = infer<typeof ClockSchema>;

export const ClockSchema: ZodMiniObject<{
	timescale: ZodMiniDefault<ZodMiniNumber<number>>;
	wall: $ZodBranded<ZodMiniNumber<number>, "u53", "out">;
}, $strip>;

export type CmafContainer = Extract<Container, {
	kind: "cmaf";
}>;

export import Color = Video.Color;

export import ColorSchema = Video.ColorSchema;

export type Container = infer<typeof ContainerSchema>;

export const ContainerSchema: ZodMiniDefault<ZodMiniUnion<readonly [ZodMiniDiscriminatedUnion<[ZodMiniObject<{
	kind: ZodMiniLiteral<"legacy">;
}, $strip>, ZodMiniObject<{
	init: ZodMiniBase64;
	kind: ZodMiniLiteral<"cmaf">;
}, $strip>, ZodMiniObject<{
	kind: ZodMiniLiteral<"loc">;
}, $strip>], "kind">, ZodMiniPipe<ZodMiniObject<{
	kind: ZodMiniString<string>;
}, $loose>, ZodMiniTransform<{
	kind: "unknown";
	raw: {
		[x: string]: unknown;
		kind: string;
	};
}, {
	[x: string]: unknown;
	kind: string;
}>>]>>;

export const DEFAULT_FORMAT: Format;

export class DuplicateRendition extends Error {
	constructor(id: string);
	readonly id: string;
}

export type Echo = infer<typeof EchoSchema>;

export const EchoSchema: ZodMiniObject<{
	path: ZodMiniType<RelativeBroadcast, string, $ZodTypeInternals<RelativeBroadcast, string>>;
}, $strip>;

export const EpochSchema: ZodMiniType<ReferenceEpoch, string>;

export class EscapingBroadcast extends Error {
	constructor(broadcast: RelativeBroadcast);
	readonly broadcast: RelativeBroadcast;
}

export const FORMATS: readonly ["hang", "msf"];

export type Format = typeof FORMATS[number];

export type Hex = infer<typeof hexSchema>;

export type Hls = infer<typeof HlsSchema>;

export const HlsFieldSchema: ZodMiniCatch<ZodMiniOptional<ZodMiniObject<{
	duration: $ZodBranded<ZodMiniNumber<number>, "u53", "out">;
	track: ZodMiniString<string>;
}, $strip>>>;

export const HlsSchema: ZodMiniObject<{
	duration: $ZodBranded<ZodMiniNumber<number>, "u53", "out">;
	track: ZodMiniString<string>;
}, $strip>;

export type Json = infer<typeof JsonSchema>;

export type JsonConfig = infer<typeof JsonConfigSchema>;

export const JsonConfigSchema: ZodMiniObject<{
	bitrate: ZodMiniOptional<$ZodBranded<ZodMiniNumber<number>, "u53", "out">>;
	broadcast: ZodMiniOptional<ZodMiniType<RelativeBroadcast, string, $ZodTypeInternals<RelativeBroadcast, string>>>;
	delay: ZodMiniOptional<ZodMiniPipe<$ZodBranded<ZodMiniNumber<number>, "u53", "out">, ZodMiniTransform<number & $brand<"u53"> | undefined, number & $brand<"u53">>>>;
	epoch: ZodMiniOptional<ZodMiniType<ReferenceEpoch, string, $ZodTypeInternals<ReferenceEpoch, string>>>;
	jitter: ZodMiniOptional<ZodMiniPipe<$ZodBranded<ZodMiniNumber<number>, "u53", "out">, ZodMiniTransform<number & $brand<"u53"> | undefined, number & $brand<"u53">>>>;
	mode: ZodMiniString<string>;
	schema: ZodMiniOptional<ZodMiniString<string>>;
	track: ZodMiniOptional<ZodMiniString<string>>;
}, $loose>;

export const JsonSchema: ZodMiniObject<{
	tracks: ZodMiniRecord<ZodMiniString<string>, ZodMiniObject<{
		bitrate: ZodMiniOptional<$ZodBranded<ZodMiniNumber<number>, "u53", "out">>;
		broadcast: ZodMiniOptional<ZodMiniType<RelativeBroadcast, string, $ZodTypeInternals<RelativeBroadcast, string>>>;
		delay: ZodMiniOptional<ZodMiniPipe<$ZodBranded<ZodMiniNumber<number>, "u53", "out">, ZodMiniTransform<number & $brand<"u53"> | undefined, number & $brand<"u53">>>>;
		epoch: ZodMiniOptional<ZodMiniType<ReferenceEpoch, string, $ZodTypeInternals<ReferenceEpoch, string>>>;
		jitter: ZodMiniOptional<ZodMiniPipe<$ZodBranded<ZodMiniNumber<number>, "u53", "out">, ZodMiniTransform<number & $brand<"u53"> | undefined, number & $brand<"u53">>>>;
		mode: ZodMiniString<string>;
		schema: ZodMiniOptional<ZodMiniString<string>>;
		track: ZodMiniOptional<ZodMiniString<string>>;
	}, $loose>>;
}, $strip>;

export type KnownMode = "snapshot" | "stream";

export const MOQ_EPOCH_UNIX_MILLIS: 1577836800000;

export type Mode = infer<typeof ModeSchema>;

export const ModeSchema: ZodMiniString<string>;

export const PRIORITY: {
	readonly audio: 80;
	readonly catalog: 100;
	readonly text: 90;
	readonly video: 60;
};

export import Projection = Video.Projection;

export import ProjectionSchema = Video.ProjectionSchema;

export type ReferenceEpoch = string & {
	readonly _brand: "epoch";
};

export type RelativeBroadcast = string & {
	__brand: "Relative";
};

export const RelativeBroadcastSchema: ZodMiniType<RelativeBroadcast, string>;

export type Root = infer<typeof RootSchema>;

export const RootSchema: ZodMiniObject<{
	audio: ZodMiniOptional<ZodMiniObject<{
		renditions: ZodMiniPipe<ZodMiniCustom<Record<string, unknown>, Record<string, unknown>>, ZodMiniTransform<Record<string, {
			bitrate?: number & $brand<"u53"> | undefined;
			broadcast?: RelativeBroadcast | undefined;
			codec: string;
			container: {
				kind: "legacy";
			} | {
				init: string;
				kind: "cmaf";
			} | {
				kind: "loc";
			} | {
				kind: "unknown";
				raw: {
					[x: string]: unknown;
					kind: string;
				};
			};
			delay?: number & $brand<"u53"> | undefined;
			description?: string | undefined;
			enabled?: boolean | undefined;
			epoch?: ReferenceEpoch | undefined;
			hls?: {
				duration: number & $brand<"u53">;
				track: string;
			} | undefined;
			jitter?: number & $brand<"u53"> | undefined;
			label?: string | undefined;
			layout?: string | undefined;
			numberOfChannels: number & $brand<"u53">;
			sampleRate: number & $brand<"u53">;
			track?: string | undefined;
			warmup?: number & $brand<"u53"> | undefined;
		}>, Record<string, unknown>>>;
	}, $strip>>;
	binary: ZodMiniOptional<ZodMiniUnion<readonly [ZodMiniObject<{
		tracks: ZodMiniRecord<ZodMiniString<string>, ZodMiniObject<{
			bitrate: ZodMiniOptional<$ZodBranded<ZodMiniNumber<number>, "u53", "out">>;
			broadcast: ZodMiniOptional<ZodMiniType<RelativeBroadcast, string, $ZodTypeInternals<RelativeBroadcast, string>>>;
			delay: ZodMiniOptional<ZodMiniPipe<$ZodBranded<ZodMiniNumber<number>, "u53", "out">, ZodMiniTransform<number & $brand<"u53"> | undefined, number & $brand<"u53">>>>;
			epoch: ZodMiniOptional<ZodMiniType<ReferenceEpoch, string, $ZodTypeInternals<ReferenceEpoch, string>>>;
			jitter: ZodMiniOptional<ZodMiniPipe<$ZodBranded<ZodMiniNumber<number>, "u53", "out">, ZodMiniTransform<number & $brand<"u53"> | undefined, number & $brand<"u53">>>>;
			mime: ZodMiniOptional<ZodMiniString<string>>;
			mode: ZodMiniString<string>;
			track: ZodMiniOptional<ZodMiniString<string>>;
		}, $loose>>;
	}, $strip>, ZodMiniPipe<ZodMiniCustom<unknown, unknown>, ZodMiniTransform<undefined, unknown>>]>>;
	clock: ZodMiniOptional<ZodMiniObject<{
		timescale: ZodMiniDefault<ZodMiniNumber<number>>;
		wall: $ZodBranded<ZodMiniNumber<number>, "u53", "out">;
	}, $strip>>;
	echo: ZodMiniOptional<ZodMiniUnion<readonly [ZodMiniObject<{
		path: ZodMiniType<RelativeBroadcast, string, $ZodTypeInternals<RelativeBroadcast, string>>;
	}, $strip>, ZodMiniPipe<ZodMiniCustom<unknown, unknown>, ZodMiniTransform<undefined, unknown>>]>>;
	json: ZodMiniOptional<ZodMiniUnion<readonly [ZodMiniObject<{
		tracks: ZodMiniRecord<ZodMiniString<string>, ZodMiniObject<{
			bitrate: ZodMiniOptional<$ZodBranded<ZodMiniNumber<number>, "u53", "out">>;
			broadcast: ZodMiniOptional<ZodMiniType<RelativeBroadcast, string, $ZodTypeInternals<RelativeBroadcast, string>>>;
			delay: ZodMiniOptional<ZodMiniPipe<$ZodBranded<ZodMiniNumber<number>, "u53", "out">, ZodMiniTransform<number & $brand<"u53"> | undefined, number & $brand<"u53">>>>;
			epoch: ZodMiniOptional<ZodMiniType<ReferenceEpoch, string, $ZodTypeInternals<ReferenceEpoch, string>>>;
			jitter: ZodMiniOptional<ZodMiniPipe<$ZodBranded<ZodMiniNumber<number>, "u53", "out">, ZodMiniTransform<number & $brand<"u53"> | undefined, number & $brand<"u53">>>>;
			mode: ZodMiniString<string>;
			schema: ZodMiniOptional<ZodMiniString<string>>;
			track: ZodMiniOptional<ZodMiniString<string>>;
		}, $loose>>;
	}, $strip>, ZodMiniPipe<ZodMiniCustom<unknown, unknown>, ZodMiniTransform<undefined, unknown>>]>>;
	stats: ZodMiniOptional<ZodMiniUnion<readonly [ZodMiniObject<{
		track: ZodMiniString<string>;
	}, $strip>, ZodMiniPipe<ZodMiniCustom<unknown, unknown>, ZodMiniTransform<undefined, unknown>>]>>;
	text: ZodMiniOptional<ZodMiniUnion<readonly [ZodMiniObject<{
		renditions: ZodMiniRecord<ZodMiniString<string>, ZodMiniObject<{
			broadcast: ZodMiniOptional<ZodMiniType<RelativeBroadcast, string, $ZodTypeInternals<RelativeBroadcast, string>>>;
			container: ZodMiniDefault<ZodMiniUnion<readonly [ZodMiniDiscriminatedUnion<[ZodMiniObject<{
				kind: ZodMiniLiteral<"legacy">;
			}, $strip>, ZodMiniObject<{
				init: ZodMiniBase64;
				kind: ZodMiniLiteral<"cmaf">;
			}, $strip>, ZodMiniObject<{
				kind: ZodMiniLiteral<"loc">;
			}, $strip>], "kind">, ZodMiniPipe<ZodMiniObject<{
				kind: ZodMiniString<string>;
			}, $loose>, ZodMiniTransform<{
				kind: "unknown";
				raw: {
					[x: string]: unknown;
					kind: string;
				};
			}, {
				[x: string]: unknown;
				kind: string;
			}>>]>>;
			delay: ZodMiniOptional<ZodMiniPipe<$ZodBranded<ZodMiniNumber<number>, "u53", "out">, ZodMiniTransform<number & $brand<"u53"> | undefined, number & $brand<"u53">>>>;
			epoch: ZodMiniOptional<ZodMiniType<ReferenceEpoch, string, $ZodTypeInternals<ReferenceEpoch, string>>>;
			format: ZodMiniString<string>;
			jitter: ZodMiniOptional<ZodMiniPipe<$ZodBranded<ZodMiniNumber<number>, "u53", "out">, ZodMiniTransform<number & $brand<"u53"> | undefined, number & $brand<"u53">>>>;
			label: ZodMiniOptional<ZodMiniString<string>>;
			lang: ZodMiniOptional<ZodMiniString<string>>;
			role: ZodMiniDefault<ZodMiniString<string>>;
			track: ZodMiniOptional<ZodMiniString<string>>;
		}, $strip>>;
	}, $strip>, ZodMiniPipe<ZodMiniCustom<unknown, unknown>, ZodMiniTransform<undefined, unknown>>]>>;
	version: ZodMiniOptional<ZodMiniNumber<number>>;
	video: ZodMiniOptional<ZodMiniObject<{
		display: ZodMiniOptional<ZodMiniObject<{
			height: $ZodBranded<ZodMiniNumber<number>, "u53", "out">;
			width: $ZodBranded<ZodMiniNumber<number>, "u53", "out">;
		}, $strip>>;
		flip: ZodMiniOptional<ZodMiniBoolean<boolean>>;
		renditions: ZodMiniPipe<ZodMiniCustom<Record<string, unknown>, Record<string, unknown>>, ZodMiniTransform<Record<string, {
			bitrate?: number & $brand<"u53"> | undefined;
			broadcast?: RelativeBroadcast | undefined;
			codec: string;
			codedHeight?: number & $brand<"u53"> | undefined;
			codedWidth?: number & $brand<"u53"> | undefined;
			color?: {
				contentLight?: {
					maxCll: number;
					maxFall: number;
				} | undefined;
				mastering?: {
					blue: {
						x: number;
						y: number;
					};
					green: {
						x: number;
						y: number;
					};
					maxLuminance: number;
					minLuminance: number;
					red: {
						x: number;
						y: number;
					};
					white: {
						x: number;
						y: number;
					};
				} | undefined;
				matrix?: number & $brand<"u8"> | undefined;
				primaries?: number & $brand<"u8"> | undefined;
				range?: "full" | "limited" | undefined;
				transfer?: number & $brand<"u8"> | undefined;
			} | undefined;
			container: {
				kind: "legacy";
			} | {
				init: string;
				kind: "cmaf";
			} | {
				kind: "loc";
			} | {
				kind: "unknown";
				raw: {
					[x: string]: unknown;
					kind: string;
				};
			};
			delay?: number & $brand<"u53"> | undefined;
			description?: string | undefined;
			displayAspectHeight?: number & $brand<"u53"> | undefined;
			displayAspectWidth?: number & $brand<"u53"> | undefined;
			enabled?: boolean | undefined;
			epoch?: ReferenceEpoch | undefined;
			framerate?: number | undefined;
			hls?: {
				duration: number & $brand<"u53">;
				track: string;
			} | undefined;
			interlaced?: boolean | undefined;
			jitter?: number & $brand<"u53"> | undefined;
			label?: string | undefined;
			optimizeForLatency?: boolean | undefined;
			preference?: number | undefined;
			projection?: {
				kind: "rectilinear";
			} | {
				fov?: number | undefined;
				kind: "equirectangular";
			} | {
				kind: "unknown";
				raw: {
					[x: string]: unknown;
					kind: string;
				};
			} | undefined;
			reorder?: number & $brand<"u8"> | undefined;
			stereo?: string | undefined;
			track?: string | undefined;
			warmup?: number & $brand<"u53"> | undefined;
		}>, Record<string, unknown>>>;
		rotation: ZodMiniOptional<ZodMiniNumber<number>>;
	}, $strip>>;
}, $loose>;

export type Stats = infer<typeof StatsSchema>;

export const StatsSchema: ZodMiniObject<{
	track: ZodMiniString<string>;
}, $strip>;

export const TRACK: "catalog.json";

export type Text = infer<typeof TextSchema>;

export type TextConfig = infer<typeof TextConfigSchema>;

export const TextConfigSchema: ZodMiniObject<{
	broadcast: ZodMiniOptional<ZodMiniType<RelativeBroadcast, string, $ZodTypeInternals<RelativeBroadcast, string>>>;
	container: ZodMiniDefault<ZodMiniUnion<readonly [ZodMiniDiscriminatedUnion<[ZodMiniObject<{
		kind: ZodMiniLiteral<"legacy">;
	}, $strip>, ZodMiniObject<{
		init: ZodMiniBase64;
		kind: ZodMiniLiteral<"cmaf">;
	}, $strip>, ZodMiniObject<{
		kind: ZodMiniLiteral<"loc">;
	}, $strip>], "kind">, ZodMiniPipe<ZodMiniObject<{
		kind: ZodMiniString<string>;
	}, $loose>, ZodMiniTransform<{
		kind: "unknown";
		raw: {
			[x: string]: unknown;
			kind: string;
		};
	}, {
		[x: string]: unknown;
		kind: string;
	}>>]>>;
	delay: ZodMiniOptional<ZodMiniPipe<$ZodBranded<ZodMiniNumber<number>, "u53", "out">, ZodMiniTransform<number & $brand<"u53"> | undefined, number & $brand<"u53">>>>;
	epoch: ZodMiniOptional<ZodMiniType<ReferenceEpoch, string, $ZodTypeInternals<ReferenceEpoch, string>>>;
	format: ZodMiniString<string>;
	jitter: ZodMiniOptional<ZodMiniPipe<$ZodBranded<ZodMiniNumber<number>, "u53", "out">, ZodMiniTransform<number & $brand<"u53"> | undefined, number & $brand<"u53">>>>;
	label: ZodMiniOptional<ZodMiniString<string>>;
	lang: ZodMiniOptional<ZodMiniString<string>>;
	role: ZodMiniDefault<ZodMiniString<string>>;
	track: ZodMiniOptional<ZodMiniString<string>>;
}, $strip>;

export const TextFormatSchema: ZodMiniString<string>;

export type TextRole = infer<typeof TextRoleSchema>;

export const TextRoleSchema: ZodMiniString<string>;

export const TextSchema: ZodMiniObject<{
	renditions: ZodMiniRecord<ZodMiniString<string>, ZodMiniObject<{
		broadcast: ZodMiniOptional<ZodMiniType<RelativeBroadcast, string, $ZodTypeInternals<RelativeBroadcast, string>>>;
		container: ZodMiniDefault<ZodMiniUnion<readonly [ZodMiniDiscriminatedUnion<[ZodMiniObject<{
			kind: ZodMiniLiteral<"legacy">;
		}, $strip>, ZodMiniObject<{
			init: ZodMiniBase64;
			kind: ZodMiniLiteral<"cmaf">;
		}, $strip>, ZodMiniObject<{
			kind: ZodMiniLiteral<"loc">;
		}, $strip>], "kind">, ZodMiniPipe<ZodMiniObject<{
			kind: ZodMiniString<string>;
		}, $loose>, ZodMiniTransform<{
			kind: "unknown";
			raw: {
				[x: string]: unknown;
				kind: string;
			};
		}, {
			[x: string]: unknown;
			kind: string;
		}>>]>>;
		delay: ZodMiniOptional<ZodMiniPipe<$ZodBranded<ZodMiniNumber<number>, "u53", "out">, ZodMiniTransform<number & $brand<"u53"> | undefined, number & $brand<"u53">>>>;
		epoch: ZodMiniOptional<ZodMiniType<ReferenceEpoch, string, $ZodTypeInternals<ReferenceEpoch, string>>>;
		format: ZodMiniString<string>;
		jitter: ZodMiniOptional<ZodMiniPipe<$ZodBranded<ZodMiniNumber<number>, "u53", "out">, ZodMiniTransform<number & $brand<"u53"> | undefined, number & $brand<"u53">>>>;
		label: ZodMiniOptional<ZodMiniString<string>>;
		lang: ZodMiniOptional<ZodMiniString<string>>;
		role: ZodMiniDefault<ZodMiniString<string>>;
		track: ZodMiniOptional<ZodMiniString<string>>;
	}, $strip>>;
}, $strip>;

export type Track = infer<typeof TrackSchema>;

export const TrackSchema: ZodMiniObject<{
	name: ZodMiniString<string>;
}, $strip>;

export type U53 = infer<typeof u53Schema>;

export type U8 = infer<typeof u8Schema>;

export const UnknownContainerSchema: ZodMiniPipe<ZodMiniObject<{
	kind: ZodMiniString<string>;
}, $loose>, ZodMiniTransform<{
	kind: "unknown";
	raw: {
		[x: string]: unknown;
		kind: string;
	};
}, {
	[x: string]: unknown;
	kind: string;
}>>;

export class UnsupportedVersion extends Error {
	constructor(version: number);
	readonly version: number;
}

export const VERSION: 4;

export namespace Video {
	export type Color = infer<typeof Video.ColorSchema>;
	export const ColorSchema: ZodMiniObject<{
		contentLight: ZodMiniOptional<ZodMiniObject<{
			maxCll: ZodMiniNumber<number>;
			maxFall: ZodMiniNumber<number>;
		}, $strip>>;
		mastering: ZodMiniOptional<ZodMiniObject<{
			blue: ZodMiniObject<{
				x: ZodMiniNumber<number>;
				y: ZodMiniNumber<number>;
			}, $strip>;
			green: ZodMiniObject<{
				x: ZodMiniNumber<number>;
				y: ZodMiniNumber<number>;
			}, $strip>;
			maxLuminance: ZodMiniNumber<number>;
			minLuminance: ZodMiniNumber<number>;
			red: ZodMiniObject<{
				x: ZodMiniNumber<number>;
				y: ZodMiniNumber<number>;
			}, $strip>;
			white: ZodMiniObject<{
				x: ZodMiniNumber<number>;
				y: ZodMiniNumber<number>;
			}, $strip>;
		}, $strip>>;
		matrix: ZodMiniOptional<$ZodBranded<ZodMiniNumber<number>, "u8", "out">>;
		primaries: ZodMiniOptional<$ZodBranded<ZodMiniNumber<number>, "u8", "out">>;
		range: ZodMiniOptional<ZodMiniEnum<{
			full: "full";
			limited: "limited";
		}>>;
		transfer: ZodMiniOptional<$ZodBranded<ZodMiniNumber<number>, "u8", "out">>;
	}, $strip>;
	export type Projection = infer<typeof Video.ProjectionSchema>;
	export const ProjectionSchema: ZodMiniUnion<readonly [ZodMiniDiscriminatedUnion<[ZodMiniObject<{
		kind: ZodMiniLiteral<"rectilinear">;
	}, $strip>, ZodMiniObject<{
		fov: ZodMiniOptional<ZodMiniNumber<number>>;
		kind: ZodMiniLiteral<"equirectangular">;
	}, $strip>], "kind">, ZodMiniPipe<ZodMiniObject<{
		kind: ZodMiniString<string>;
	}, $loose>, ZodMiniTransform<{
		kind: "unknown";
		raw: {
			[x: string]: unknown;
			kind: string;
		};
	}, {
		[x: string]: unknown;
		kind: string;
	}>>]>;
	export type Video = infer<typeof Video.VideoSchema>;
	export type VideoConfig = infer<typeof Video.VideoConfigSchema>;
	export const VideoConfigSchema: ZodMiniObject<{
		bitrate: ZodMiniOptional<$ZodBranded<ZodMiniNumber<number>, "u53", "out">>;
		broadcast: ZodMiniOptional<ZodMiniType<RelativeBroadcast, string, $ZodTypeInternals<RelativeBroadcast, string>>>;
		codec: ZodMiniString<string>;
		codedHeight: ZodMiniOptional<$ZodBranded<ZodMiniNumber<number>, "u53", "out">>;
		codedWidth: ZodMiniOptional<$ZodBranded<ZodMiniNumber<number>, "u53", "out">>;
		color: ZodMiniOptional<ZodMiniObject<{
			contentLight: ZodMiniOptional<ZodMiniObject<{
				maxCll: ZodMiniNumber<number>;
				maxFall: ZodMiniNumber<number>;
			}, $strip>>;
			mastering: ZodMiniOptional<ZodMiniObject<{
				blue: ZodMiniObject<{
					x: ZodMiniNumber<number>;
					y: ZodMiniNumber<number>;
				}, $strip>;
				green: ZodMiniObject<{
					x: ZodMiniNumber<number>;
					y: ZodMiniNumber<number>;
				}, $strip>;
				maxLuminance: ZodMiniNumber<number>;
				minLuminance: ZodMiniNumber<number>;
				red: ZodMiniObject<{
					x: ZodMiniNumber<number>;
					y: ZodMiniNumber<number>;
				}, $strip>;
				white: ZodMiniObject<{
					x: ZodMiniNumber<number>;
					y: ZodMiniNumber<number>;
				}, $strip>;
			}, $strip>>;
			matrix: ZodMiniOptional<$ZodBranded<ZodMiniNumber<number>, "u8", "out">>;
			primaries: ZodMiniOptional<$ZodBranded<ZodMiniNumber<number>, "u8", "out">>;
			range: ZodMiniOptional<ZodMiniEnum<{
				full: "full";
				limited: "limited";
			}>>;
			transfer: ZodMiniOptional<$ZodBranded<ZodMiniNumber<number>, "u8", "out">>;
		}, $strip>>;
		container: ZodMiniDefault<ZodMiniUnion<readonly [ZodMiniDiscriminatedUnion<[ZodMiniObject<{
			kind: ZodMiniLiteral<"legacy">;
		}, $strip>, ZodMiniObject<{
			init: ZodMiniBase64;
			kind: ZodMiniLiteral<"cmaf">;
		}, $strip>, ZodMiniObject<{
			kind: ZodMiniLiteral<"loc">;
		}, $strip>], "kind">, ZodMiniPipe<ZodMiniObject<{
			kind: ZodMiniString<string>;
		}, $loose>, ZodMiniTransform<{
			kind: "unknown";
			raw: {
				[x: string]: unknown;
				kind: string;
			};
		}, {
			[x: string]: unknown;
			kind: string;
		}>>]>>;
		delay: ZodMiniOptional<ZodMiniPipe<$ZodBranded<ZodMiniNumber<number>, "u53", "out">, ZodMiniTransform<number & $brand<"u53"> | undefined, number & $brand<"u53">>>>;
		description: ZodMiniOptional<ZodMiniString<string>>;
		displayAspectHeight: ZodMiniOptional<$ZodBranded<ZodMiniNumber<number>, "u53", "out">>;
		displayAspectWidth: ZodMiniOptional<$ZodBranded<ZodMiniNumber<number>, "u53", "out">>;
		enabled: ZodMiniOptional<ZodMiniBoolean<boolean>>;
		epoch: ZodMiniOptional<ZodMiniType<ReferenceEpoch, string, $ZodTypeInternals<ReferenceEpoch, string>>>;
		framerate: ZodMiniOptional<ZodMiniNumber<number>>;
		hls: ZodMiniCatch<ZodMiniOptional<ZodMiniObject<{
			duration: $ZodBranded<ZodMiniNumber<number>, "u53", "out">;
			track: ZodMiniString<string>;
		}, $strip>>>;
		interlaced: ZodMiniOptional<ZodMiniBoolean<boolean>>;
		jitter: ZodMiniOptional<ZodMiniPipe<$ZodBranded<ZodMiniNumber<number>, "u53", "out">, ZodMiniTransform<number & $brand<"u53"> | undefined, number & $brand<"u53">>>>;
		label: ZodMiniOptional<ZodMiniString<string>>;
		optimizeForLatency: ZodMiniOptional<ZodMiniBoolean<boolean>>;
		preference: ZodMiniOptional<ZodMiniInt32>;
		projection: ZodMiniOptional<ZodMiniUnion<readonly [ZodMiniDiscriminatedUnion<[ZodMiniObject<{
			kind: ZodMiniLiteral<"rectilinear">;
		}, $strip>, ZodMiniObject<{
			fov: ZodMiniOptional<ZodMiniNumber<number>>;
			kind: ZodMiniLiteral<"equirectangular">;
		}, $strip>], "kind">, ZodMiniPipe<ZodMiniObject<{
			kind: ZodMiniString<string>;
		}, $loose>, ZodMiniTransform<{
			kind: "unknown";
			raw: {
				[x: string]: unknown;
				kind: string;
			};
		}, {
			[x: string]: unknown;
			kind: string;
		}>>]>>;
		reorder: ZodMiniOptional<$ZodBranded<ZodMiniNumber<number>, "u8", "out">>;
		stereo: ZodMiniOptional<ZodMiniString<string>>;
		track: ZodMiniOptional<ZodMiniString<string>>;
		warmup: ZodMiniOptional<$ZodBranded<ZodMiniNumber<number>, "u53", "out">>;
	}, $strip>;
	export const VideoSchema: ZodMiniObject<{
		display: ZodMiniOptional<ZodMiniObject<{
			height: $ZodBranded<ZodMiniNumber<number>, "u53", "out">;
			width: $ZodBranded<ZodMiniNumber<number>, "u53", "out">;
		}, $strip>>;
		flip: ZodMiniOptional<ZodMiniBoolean<boolean>>;
		renditions: ZodMiniPipe<ZodMiniCustom<Record<string, unknown>, Record<string, unknown>>, ZodMiniTransform<Record<string, {
			bitrate?: number & $brand<"u53"> | undefined;
			broadcast?: RelativeBroadcast | undefined;
			codec: string;
			codedHeight?: number & $brand<"u53"> | undefined;
			codedWidth?: number & $brand<"u53"> | undefined;
			color?: {
				contentLight?: {
					maxCll: number;
					maxFall: number;
				} | undefined;
				mastering?: {
					blue: {
						x: number;
						y: number;
					};
					green: {
						x: number;
						y: number;
					};
					maxLuminance: number;
					minLuminance: number;
					red: {
						x: number;
						y: number;
					};
					white: {
						x: number;
						y: number;
					};
				} | undefined;
				matrix?: number & $brand<"u8"> | undefined;
				primaries?: number & $brand<"u8"> | undefined;
				range?: "full" | "limited" | undefined;
				transfer?: number & $brand<"u8"> | undefined;
			} | undefined;
			container: {
				kind: "legacy";
			} | {
				init: string;
				kind: "cmaf";
			} | {
				kind: "loc";
			} | {
				kind: "unknown";
				raw: {
					[x: string]: unknown;
					kind: string;
				};
			};
			delay?: number & $brand<"u53"> | undefined;
			description?: string | undefined;
			displayAspectHeight?: number & $brand<"u53"> | undefined;
			displayAspectWidth?: number & $brand<"u53"> | undefined;
			enabled?: boolean | undefined;
			epoch?: ReferenceEpoch | undefined;
			framerate?: number | undefined;
			hls?: {
				duration: number & $brand<"u53">;
				track: string;
			} | undefined;
			interlaced?: boolean | undefined;
			jitter?: number & $brand<"u53"> | undefined;
			label?: string | undefined;
			optimizeForLatency?: boolean | undefined;
			preference?: number | undefined;
			projection?: {
				kind: "rectilinear";
			} | {
				fov?: number | undefined;
				kind: "equirectangular";
			} | {
				kind: "unknown";
				raw: {
					[x: string]: unknown;
					kind: string;
				};
			} | undefined;
			reorder?: number & $brand<"u8"> | undefined;
			stereo?: string | undefined;
			track?: string | undefined;
			warmup?: number & $brand<"u53"> | undefined;
		}>, Record<string, unknown>>>;
		rotation: ZodMiniOptional<ZodMiniNumber<number>>;
	}, $strip>;
	export function ranked(renditions: Record<string, Video.VideoConfig>): [string, Video.VideoConfig][];
}

export type Video = Video.Video;

export import VideoConfig = Video.VideoConfig;

export import VideoConfigSchema = Video.VideoConfigSchema;

export import VideoSchema = Video.VideoSchema;

export function checkRenditions(root: Root): Root;

export function checkResolvable(root: Root, base: Net.Path.Valid): Root;

export function containerSupported(container: Container): boolean;

export function detectFormat(name: string): Format | undefined;

export function encode(root: Root): Root;

export const hexSchema: ZodMiniString<string>;

export function modeSupported(mode: Mode): mode is KnownMode;

export const nonzeroU53Schema: $ZodBranded<ZodMiniNumber<number>, "u53", "out">;

export function track(id: string, config: {
	track?: string;
}): string;

export function u53(value: number): U53;

export const u53Schema: $ZodBranded<ZodMiniNumber<number>, "u53", "out">;

export function u8(value: number): U8;

export const u8Schema: $ZodBranded<ZodMiniNumber<number>, "u8", "out">;

export function wallClockTime(clock: Clock, pts: number, ptsTimescale: number): Date;

export function watch(broadcast: Net.Broadcast.Consumer): AsyncIterable<Root>;
```
