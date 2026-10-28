# `@moq/watch/support`

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
		decoding: Audio;
		render: boolean;
	};
	video: {
		decoding: Video | undefined;
		render: boolean;
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
