# `@moq/hang/catalog`

```ts
import { $ZodBranded } from "zod/v4/core";
import { $ZodTypeInternals } from "zod/v4/core";
import { $brand } from "zod/mini";
import { $loose } from "zod/v4/core";
import { $strip } from "zod/v4/core";
import { Net } from "@moq/hang";
import { ZodMiniArray } from "zod/mini";
import { ZodMiniBase64 } from "zod/mini";
import { ZodMiniBoolean } from "zod/mini";
import { ZodMiniCatch } from "zod/mini";
import { ZodMiniCustom } from "zod/mini";
import { ZodMiniDefault } from "zod/mini";
import { ZodMiniDiscriminatedUnion } from "zod/mini";
import { ZodMiniEnum } from "zod/mini";
import { ZodMiniGUID } from "zod/mini";
import { ZodMiniInt32 } from "zod/mini";
import { ZodMiniLiteral } from "zod/mini";
import { ZodMiniNumber } from "zod/mini";
import { ZodMiniObject } from "zod/mini";
import { ZodMiniOptional } from "zod/mini";
import { ZodMiniPipe } from "zod/mini";
import { ZodMiniRecord } from "zod/mini";
import { ZodMiniString } from "zod/mini";
import { ZodMiniTransform } from "zod/mini";
import { ZodMiniType } from "zod/mini";
import { ZodMiniURL } from "zod/mini";
import { ZodMiniUnion } from "zod/mini";
import { ZodMiniUnknown } from "zod/mini";
import { infer } from "zod/mini";

export const Audio: {
	ranked: (renditions: Record<string, AudioConfig>) => [string, AudioConfig][];
};

export type Audio = infer<typeof AudioSchema>;

export type AudioConfig = infer<typeof AudioConfigSchema>;

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
	drm: ZodMiniOptional<ZodMiniArray<ZodMiniString<string>>>;
	enabled: ZodMiniOptional<ZodMiniBoolean<boolean>>;
	instance: ZodMiniOptional<ZodMiniType<ReferenceInstance, string, $ZodTypeInternals<ReferenceInstance, string>>>;
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
		drm?: string[] | undefined;
		enabled?: boolean | undefined;
		instance?: ReferenceInstance | undefined;
		jitter?: number & $brand<"u53"> | undefined;
		label?: string | undefined;
		layout?: string | undefined;
		numberOfChannels: number & $brand<"u53">;
		sampleRate: number & $brand<"u53">;
		track?: string | undefined;
		warmup?: number & $brand<"u53"> | undefined;
	}>, Record<string, unknown>>>;
}, $strip>;

export type Binary = infer<typeof BinarySchema>;

export type BinaryConfig = infer<typeof BinaryConfigSchema>;

export const BinaryConfigSchema: ZodMiniObject<{
	bitrate: ZodMiniOptional<$ZodBranded<ZodMiniNumber<number>, "u53", "out">>;
	broadcast: ZodMiniOptional<ZodMiniType<RelativeBroadcast, string, $ZodTypeInternals<RelativeBroadcast, string>>>;
	delay: ZodMiniOptional<ZodMiniPipe<$ZodBranded<ZodMiniNumber<number>, "u53", "out">, ZodMiniTransform<number & $brand<"u53"> | undefined, number & $brand<"u53">>>>;
	instance: ZodMiniOptional<ZodMiniType<ReferenceInstance, string, $ZodTypeInternals<ReferenceInstance, string>>>;
	jitter: ZodMiniOptional<ZodMiniPipe<$ZodBranded<ZodMiniNumber<number>, "u53", "out">, ZodMiniTransform<number & $brand<"u53"> | undefined, number & $brand<"u53">>>>;
	mime: ZodMiniOptional<ZodMiniString<string>>;
	mode: ZodMiniString<string>;
	track: ZodMiniOptional<ZodMiniString<string>>;
}, $loose>;

export const BinarySchema: ZodMiniObject<{
	tracks: ZodMiniPipe<ZodMiniCustom<Record<string, unknown>, Record<string, unknown>>, ZodMiniTransform<Record<string, {
		[x: string]: unknown;
		bitrate?: number & $brand<"u53"> | undefined;
		broadcast?: RelativeBroadcast | undefined;
		delay?: number & $brand<"u53"> | undefined;
		instance?: ReferenceInstance | undefined;
		jitter?: number & $brand<"u53"> | undefined;
		mime?: string | undefined;
		mode: string;
		track?: string | undefined;
	}>, Record<string, unknown>>>;
}, $strip>;

export type Clock = infer<typeof ClockSchema>;

export const ClockSchema: ZodMiniObject<{
	timescale: ZodMiniDefault<ZodMiniNumber<number>>;
	wall: $ZodBranded<ZodMiniNumber<number>, "u53", "out">;
}, $strip>;

export type CmafContainer = Extract<Container, {
	kind: "cmaf";
}>;

export type Color = infer<typeof ColorSchema>;

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

export const DRM_SYSTEM: {
	readonly clearkey: "1077efec-c0b2-4d02-ace3-3c1e52e2fb4b";
	readonly fairplay: "94ce86fb-07ff-4f43-adb8-93d2fa968ca2";
	readonly playready: "9a04f079-9840-4286-ab92-e65be0885f95";
	readonly widevine: "edef8ba9-79d6-4ace-a3c8-27dcd51d21ed";
};

export type DrmEntry = infer<typeof DrmEntrySchema>;

export const DrmEntrySchema: ZodMiniUnion<readonly [ZodMiniObject<{
	certificate: ZodMiniOptional<ZodMiniObject<{
		type: ZodMiniOptional<ZodMiniString<string>>;
		url: ZodMiniURL;
	}, $strip>>;
	kid: ZodMiniGUID;
	license: ZodMiniOptional<ZodMiniObject<{
		type: ZodMiniOptional<ZodMiniString<string>>;
		url: ZodMiniURL;
	}, $strip>>;
	pssh: ZodMiniOptional<ZodMiniBase64>;
	scheme: ZodMiniString<string>;
	system: ZodMiniGUID;
}, $strip>, ZodMiniRecord<ZodMiniString<string>, ZodMiniUnknown>]>;

export type DrmServer = infer<typeof DrmServerSchema>;

export const DrmServerSchema: ZodMiniObject<{
	type: ZodMiniOptional<ZodMiniString<string>>;
	url: ZodMiniURL;
}, $strip>;

export type DrmSystem = infer<typeof DrmSystemSchema>;

export const DrmSystemSchema: ZodMiniObject<{
	certificate: ZodMiniOptional<ZodMiniObject<{
		type: ZodMiniOptional<ZodMiniString<string>>;
		url: ZodMiniURL;
	}, $strip>>;
	kid: ZodMiniGUID;
	license: ZodMiniOptional<ZodMiniObject<{
		type: ZodMiniOptional<ZodMiniString<string>>;
		url: ZodMiniURL;
	}, $strip>>;
	pssh: ZodMiniOptional<ZodMiniBase64>;
	scheme: ZodMiniString<string>;
	system: ZodMiniGUID;
}, $strip>;

export class DuplicateRendition extends Error {
	constructor(id: string);
	readonly id: string;
}

export type Echo = infer<typeof EchoSchema>;

export const EchoSchema: ZodMiniObject<{
	path: ZodMiniType<RelativeBroadcast, string, $ZodTypeInternals<RelativeBroadcast, string>>;
}, $strip>;

export class EscapingBroadcast extends Error {
	constructor(broadcast: RelativeBroadcast);
	readonly broadcast: RelativeBroadcast;
}

export const FORMATS: readonly ["hang", "msf"];

export type Format = typeof FORMATS[number];

export type Hex = infer<typeof hexSchema>;

export type Include = infer<typeof IncludeSchema>;

export const IncludeSchema: ZodMiniObject<{
	broadcast: ZodMiniType<RelativeBroadcast, string, $ZodTypeInternals<RelativeBroadcast, string>>;
	instance: ZodMiniOptional<ZodMiniType<ReferenceInstance, string, $ZodTypeInternals<ReferenceInstance, string>>>;
}, $strip>;

export const InstanceSchema: ZodMiniType<ReferenceInstance, string>;

export type Join = infer<typeof JoinSchema>;

export const JoinSchema: ZodMiniObject<{
	interval: $ZodBranded<ZodMiniNumber<number>, "u53", "out">;
}, $strip>;

export type Json = infer<typeof JsonSchema>;

export type JsonConfig = infer<typeof JsonConfigSchema>;

export const JsonConfigSchema: ZodMiniObject<{
	bitrate: ZodMiniOptional<$ZodBranded<ZodMiniNumber<number>, "u53", "out">>;
	broadcast: ZodMiniOptional<ZodMiniType<RelativeBroadcast, string, $ZodTypeInternals<RelativeBroadcast, string>>>;
	delay: ZodMiniOptional<ZodMiniPipe<$ZodBranded<ZodMiniNumber<number>, "u53", "out">, ZodMiniTransform<number & $brand<"u53"> | undefined, number & $brand<"u53">>>>;
	instance: ZodMiniOptional<ZodMiniType<ReferenceInstance, string, $ZodTypeInternals<ReferenceInstance, string>>>;
	jitter: ZodMiniOptional<ZodMiniPipe<$ZodBranded<ZodMiniNumber<number>, "u53", "out">, ZodMiniTransform<number & $brand<"u53"> | undefined, number & $brand<"u53">>>>;
	mode: ZodMiniString<string>;
	schema: ZodMiniOptional<ZodMiniString<string>>;
	track: ZodMiniOptional<ZodMiniString<string>>;
}, $loose>;

export const JsonSchema: ZodMiniObject<{
	tracks: ZodMiniPipe<ZodMiniCustom<Record<string, unknown>, Record<string, unknown>>, ZodMiniTransform<Record<string, {
		[x: string]: unknown;
		bitrate?: number & $brand<"u53"> | undefined;
		broadcast?: RelativeBroadcast | undefined;
		delay?: number & $brand<"u53"> | undefined;
		instance?: ReferenceInstance | undefined;
		jitter?: number & $brand<"u53"> | undefined;
		mode: string;
		schema?: string | undefined;
		track?: string | undefined;
	}>, Record<string, unknown>>>;
}, $strip>;

export type KnownMode = "snapshot" | "stream";

export const MOQ_EPOCH_UNIX_MILLIS: 1577836800000;

export type Media = infer<typeof MediaSchema>;

export const MediaSchema: ZodMiniObject<{
	clock: ZodMiniOptional<ZodMiniObject<{
		timescale: ZodMiniDefault<ZodMiniNumber<number>>;
		wall: $ZodBranded<ZodMiniNumber<number>, "u53", "out">;
	}, $strip>>;
	join: ZodMiniCatch<ZodMiniOptional<ZodMiniObject<{
		interval: $ZodBranded<ZodMiniNumber<number>, "u53", "out">;
	}, $strip>>>;
}, $strip>;

export class MissingDrm extends Error {
	constructor(rendition: string, id: string);
	readonly id: string;
	readonly rendition: string;
}

export type Mode = infer<typeof ModeSchema>;

export const ModeSchema: ZodMiniString<string>;

export const PRIORITY: {
	readonly audio: 80;
	readonly catalog: 100;
	readonly text: 90;
	readonly video: 60;
};

export type Projection = infer<typeof ProjectionSchema>;

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

export type ReferenceInstance = string & {
	readonly _brand: "instance";
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
			drm?: string[] | undefined;
			enabled?: boolean | undefined;
			instance?: ReferenceInstance | undefined;
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
		tracks: ZodMiniPipe<ZodMiniCustom<Record<string, unknown>, Record<string, unknown>>, ZodMiniTransform<Record<string, {
			[x: string]: unknown;
			bitrate?: number & $brand<"u53"> | undefined;
			broadcast?: RelativeBroadcast | undefined;
			delay?: number & $brand<"u53"> | undefined;
			instance?: ReferenceInstance | undefined;
			jitter?: number & $brand<"u53"> | undefined;
			mime?: string | undefined;
			mode: string;
			track?: string | undefined;
		}>, Record<string, unknown>>>;
	}, $strip>, ZodMiniPipe<ZodMiniCustom<unknown, unknown>, ZodMiniTransform<undefined, unknown>>]>>;
	drm: ZodMiniOptional<ZodMiniPipe<ZodMiniCustom<Record<string, unknown>, Record<string, unknown>>, ZodMiniTransform<Record<string, Record<string, unknown> | {
		certificate?: {
			type?: string | undefined;
			url: string;
		} | undefined;
		kid: string;
		license?: {
			type?: string | undefined;
			url: string;
		} | undefined;
		pssh?: string | undefined;
		scheme: string;
		system: string;
	}>, Record<string, unknown>>>>;
	echo: ZodMiniOptional<ZodMiniUnion<readonly [ZodMiniObject<{
		path: ZodMiniType<RelativeBroadcast, string, $ZodTypeInternals<RelativeBroadcast, string>>;
	}, $strip>, ZodMiniPipe<ZodMiniCustom<unknown, unknown>, ZodMiniTransform<undefined, unknown>>]>>;
	include: ZodMiniOptional<ZodMiniPipe<ZodMiniCustom<Record<string, unknown>, Record<string, unknown>>, ZodMiniTransform<Record<string, {
		broadcast: RelativeBroadcast;
		instance?: ReferenceInstance | undefined;
	}>, Record<string, unknown>>>>;
	json: ZodMiniOptional<ZodMiniUnion<readonly [ZodMiniObject<{
		tracks: ZodMiniPipe<ZodMiniCustom<Record<string, unknown>, Record<string, unknown>>, ZodMiniTransform<Record<string, {
			[x: string]: unknown;
			bitrate?: number & $brand<"u53"> | undefined;
			broadcast?: RelativeBroadcast | undefined;
			delay?: number & $brand<"u53"> | undefined;
			instance?: ReferenceInstance | undefined;
			jitter?: number & $brand<"u53"> | undefined;
			mode: string;
			schema?: string | undefined;
			track?: string | undefined;
		}>, Record<string, unknown>>>;
	}, $strip>, ZodMiniPipe<ZodMiniCustom<unknown, unknown>, ZodMiniTransform<undefined, unknown>>]>>;
	media: ZodMiniOptional<ZodMiniUnion<readonly [ZodMiniObject<{
		clock: ZodMiniOptional<ZodMiniObject<{
			timescale: ZodMiniDefault<ZodMiniNumber<number>>;
			wall: $ZodBranded<ZodMiniNumber<number>, "u53", "out">;
		}, $strip>>;
		join: ZodMiniCatch<ZodMiniOptional<ZodMiniObject<{
			interval: $ZodBranded<ZodMiniNumber<number>, "u53", "out">;
		}, $strip>>>;
	}, $strip>, ZodMiniPipe<ZodMiniCustom<unknown, unknown>, ZodMiniTransform<undefined, unknown>>]>>;
	stats: ZodMiniOptional<ZodMiniUnion<readonly [ZodMiniObject<{
		track: ZodMiniString<string>;
	}, $strip>, ZodMiniPipe<ZodMiniCustom<unknown, unknown>, ZodMiniTransform<undefined, unknown>>]>>;
	text: ZodMiniOptional<ZodMiniUnion<readonly [ZodMiniObject<{
		renditions: ZodMiniPipe<ZodMiniCustom<Record<string, unknown>, Record<string, unknown>>, ZodMiniTransform<Record<string, {
			broadcast?: RelativeBroadcast | undefined;
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
			format: string;
			instance?: ReferenceInstance | undefined;
			jitter?: number & $brand<"u53"> | undefined;
			label?: string | undefined;
			lang?: string | undefined;
			role: string;
			track?: string | undefined;
		}>, Record<string, unknown>>>;
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
			drm?: string[] | undefined;
			enabled?: boolean | undefined;
			framerate?: number | undefined;
			instance?: ReferenceInstance | undefined;
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
	format: ZodMiniString<string>;
	instance: ZodMiniOptional<ZodMiniType<ReferenceInstance, string, $ZodTypeInternals<ReferenceInstance, string>>>;
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
	renditions: ZodMiniPipe<ZodMiniCustom<Record<string, unknown>, Record<string, unknown>>, ZodMiniTransform<Record<string, {
		broadcast?: RelativeBroadcast | undefined;
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
		format: string;
		instance?: ReferenceInstance | undefined;
		jitter?: number & $brand<"u53"> | undefined;
		label?: string | undefined;
		lang?: string | undefined;
		role: string;
		track?: string | undefined;
	}>, Record<string, unknown>>>;
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

export const Video: {
	ranked: (renditions: Record<string, VideoConfig>) => [string, VideoConfig][];
};

export type Video = infer<typeof VideoSchema>;

export type VideoConfig = infer<typeof VideoConfigSchema>;

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
	drm: ZodMiniOptional<ZodMiniArray<ZodMiniString<string>>>;
	enabled: ZodMiniOptional<ZodMiniBoolean<boolean>>;
	framerate: ZodMiniOptional<ZodMiniNumber<number>>;
	instance: ZodMiniOptional<ZodMiniType<ReferenceInstance, string, $ZodTypeInternals<ReferenceInstance, string>>>;
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
		drm?: string[] | undefined;
		enabled?: boolean | undefined;
		framerate?: number | undefined;
		instance?: ReferenceInstance | undefined;
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

export function checkRenditions(root: Root): Root;

export function checkResolvable(root: Root, base: Net.Path.Valid): Root;

export function containerSupported(container: Container): boolean;

export function detectFormat(name: string): Format | undefined;

export function encode(root: Root): Root;

export const hexSchema: ZodMiniString<string>;

export function isProtected(config: {
	container?: Container;
	drm?: string[];
}): boolean;

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
