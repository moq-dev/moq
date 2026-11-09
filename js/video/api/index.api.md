# `@moq/video`

```ts
import { Getter } from "@moq/signals";
import { Inputs } from "@moq/signals";
import { Readonlys } from "@moq/signals";
import { Signal } from "@moq/signals";

export type Backend = "auto" | "webgpu" | "2d";

export type Canvas = HTMLCanvasElement | OffscreenCanvas;

export class Renderer {
	constructor(props?: RendererProps);
	close(): void;
	readonly in: Readonlys<RendererInput>;
	readonly out: Readonlys<RendererOutput>;
}

export type RendererInput = {
	backend: Getter<Backend>;
	canvas: Getter<Canvas | undefined>;
	display: Getter<{
		height: number;
		width: number;
	} | undefined>;
	frame: Getter<VideoFrame | undefined>;
	presentation: Getter<{
		flip?: boolean;
		interlaced?: boolean;
		rotation?: number;
	} | undefined>;
};

export type RendererOutput = {
	error: Signal<Error | undefined>;
	frame: Signal<VideoFrame | undefined>;
};

export type RendererProps = Inputs<RendererInput>;
```
