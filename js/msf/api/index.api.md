# `@moq/msf`

```ts
import { $strip } from "zod/v4/core";
import { ZodMiniArray } from "@zod/mini";
import { ZodMiniBoolean } from "@zod/mini";
import { ZodMiniEnum } from "@zod/mini";
import { ZodMiniNumber } from "@zod/mini";
import { ZodMiniObject } from "@zod/mini";
import { ZodMiniOptional } from "@zod/mini";
import { ZodMiniRecord } from "@zod/mini";
import { ZodMiniString } from "@zod/mini";
import { ZodMiniUnion } from "@zod/mini";
import { ZodMiniUnknown } from "@zod/mini";
import { infer } from "@zod/mini";

export type Catalog = infer<typeof CatalogSchema>;

export const CatalogSchema: ZodMiniObject<{
	ext: ZodMiniOptional<ZodMiniRecord<ZodMiniString<string>, ZodMiniUnknown>>;
	tracks: ZodMiniArray<ZodMiniObject<{
		altGroup: ZodMiniOptional<ZodMiniNumber<number>>;
		bitrate: ZodMiniOptional<ZodMiniNumber<number>>;
		channelConfig: ZodMiniOptional<ZodMiniString<string>>;
		codec: ZodMiniOptional<ZodMiniString<string>>;
		delay: ZodMiniOptional<ZodMiniNumber<number>>;
		framerate: ZodMiniOptional<ZodMiniNumber<number>>;
		height: ZodMiniOptional<ZodMiniNumber<number>>;
		initData: ZodMiniOptional<ZodMiniString<string>>;
		isLive: ZodMiniOptional<ZodMiniBoolean<boolean>>;
		jitter: ZodMiniOptional<ZodMiniNumber<number>>;
		label: ZodMiniOptional<ZodMiniString<string>>;
		lang: ZodMiniOptional<ZodMiniString<string>>;
		locmafVersion: ZodMiniOptional<ZodMiniString<string>>;
		maxGrpSapStartingType: ZodMiniOptional<ZodMiniNumber<number>>;
		maxObjSapStartingType: ZodMiniOptional<ZodMiniNumber<number>>;
		name: ZodMiniString<string>;
		namespace: ZodMiniOptional<ZodMiniString<string>>;
		packaging: ZodMiniUnion<readonly [ZodMiniEnum<{
			cmaf: "cmaf";
			eventtimeline: "eventtimeline";
			legacy: "legacy";
			loc: "loc";
			locmaf: "locmaf";
			mediatimeline: "mediatimeline";
		}>, ZodMiniString<string>]>;
		renderGroup: ZodMiniOptional<ZodMiniNumber<number>>;
		role: ZodMiniOptional<ZodMiniUnion<readonly [ZodMiniEnum<{
			audio: "audio";
			audiodescription: "audiodescription";
			caption: "caption";
			signlanguage: "signlanguage";
			subtitle: "subtitle";
			video: "video";
		}>, ZodMiniString<string>]>>;
		samplerate: ZodMiniOptional<ZodMiniNumber<number>>;
		width: ZodMiniOptional<ZodMiniNumber<number>>;
	}, $strip>>;
}, $strip>;

export type Packaging = infer<typeof PackagingSchema>;

export const PackagingSchema: ZodMiniUnion<readonly [ZodMiniEnum<{
	cmaf: "cmaf";
	eventtimeline: "eventtimeline";
	legacy: "legacy";
	loc: "loc";
	locmaf: "locmaf";
	mediatimeline: "mediatimeline";
}>, ZodMiniString<string>]>;

export type Role = infer<typeof RoleSchema>;

export const RoleSchema: ZodMiniUnion<readonly [ZodMiniEnum<{
	audio: "audio";
	audiodescription: "audiodescription";
	caption: "caption";
	signlanguage: "signlanguage";
	subtitle: "subtitle";
	video: "video";
}>, ZodMiniString<string>]>;

export type Track = infer<typeof TrackSchema>;

export const TrackSchema: ZodMiniObject<{
	altGroup: ZodMiniOptional<ZodMiniNumber<number>>;
	bitrate: ZodMiniOptional<ZodMiniNumber<number>>;
	channelConfig: ZodMiniOptional<ZodMiniString<string>>;
	codec: ZodMiniOptional<ZodMiniString<string>>;
	delay: ZodMiniOptional<ZodMiniNumber<number>>;
	framerate: ZodMiniOptional<ZodMiniNumber<number>>;
	height: ZodMiniOptional<ZodMiniNumber<number>>;
	initData: ZodMiniOptional<ZodMiniString<string>>;
	isLive: ZodMiniOptional<ZodMiniBoolean<boolean>>;
	jitter: ZodMiniOptional<ZodMiniNumber<number>>;
	label: ZodMiniOptional<ZodMiniString<string>>;
	lang: ZodMiniOptional<ZodMiniString<string>>;
	locmafVersion: ZodMiniOptional<ZodMiniString<string>>;
	maxGrpSapStartingType: ZodMiniOptional<ZodMiniNumber<number>>;
	maxObjSapStartingType: ZodMiniOptional<ZodMiniNumber<number>>;
	name: ZodMiniString<string>;
	namespace: ZodMiniOptional<ZodMiniString<string>>;
	packaging: ZodMiniUnion<readonly [ZodMiniEnum<{
		cmaf: "cmaf";
		eventtimeline: "eventtimeline";
		legacy: "legacy";
		loc: "loc";
		locmaf: "locmaf";
		mediatimeline: "mediatimeline";
	}>, ZodMiniString<string>]>;
	renderGroup: ZodMiniOptional<ZodMiniNumber<number>>;
	role: ZodMiniOptional<ZodMiniUnion<readonly [ZodMiniEnum<{
		audio: "audio";
		audiodescription: "audiodescription";
		caption: "caption";
		signlanguage: "signlanguage";
		subtitle: "subtitle";
		video: "video";
	}>, ZodMiniString<string>]>>;
	samplerate: ZodMiniOptional<ZodMiniNumber<number>>;
	width: ZodMiniOptional<ZodMiniNumber<number>>;
}, $strip>;

export const VERSION: "draft-01";

export function decode(raw: Uint8Array): Catalog;

export function encode(catalog: Catalog): Uint8Array;

export function fetch(track: import("@moq/net").Track.Ordered): Promise<Catalog | undefined>;
```
