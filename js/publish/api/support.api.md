# `@moq/publish/support`

```ts
export type Audio = {
	aac: Level;
	opus: Level;
};

export type Codec = {
	hardware?: boolean;
	software: boolean;
};

export type Full = {
	audio: {
		capture: boolean;
		encoding: Audio;
	};
	video: {
		capture: Level;
		encoding: Video | undefined;
	};
	webtransport: Level;
};

export type Level = "full" | "partial" | "none";

export type Video = {
	av1: Codec;
	h264: Codec;
	h265: Codec;
	vp8: Codec;
	vp9: Codec;
};

export function isSupported(): Promise<Full>;
```
