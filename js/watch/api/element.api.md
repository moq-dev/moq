# `@moq/watch/element`

```ts
import { $brand } from "zod/v4/core/core.d.cts";
import { Audio } from "@moq/watch";
import { Broadcast } from "@moq/watch";
import { Clock } from "@moq/watch";
import { Delay } from "@moq/watch";
import { Hang } from "@moq/watch";
import { Net } from "@moq/watch";
import { Player } from "@moq/watch";
import { Signals } from "@moq/watch";
import { Sync } from "@moq/watch";
import { Text } from "@moq/watch";
import { Video } from "@moq/watch";

export default class extends HTMLElement {
	constructor();
	get announced(): boolean;
	set announced(value: boolean);
	attributeChangedCallback(name: "delay" | "buffer" | "url" | "name" | "announced" | "volume" | "muted" | "paused" | "visible" | "backend" | "versions" | "catalog-format" | "captions", oldValue: string | null, newValue: string | null): void;
	readonly audio: Audio.Decoder;
	get backend(): Video.Backend;
	set backend(value: Video.Backend);
	readonly broadcast: Broadcast;
	get buffer(): Clock.Milli;
	set buffer(value: Clock.Milli);
	get captions(): string | undefined;
	set captions(value: string | undefined);
	get catalog(): {
		[key: string]: unknown;
		audio?: {
			renditions: {
				[key: string]: {
					bitrate?: number & $brand<"u53">;
					broadcast?: Hang.Catalog.RelativeBroadcast;
					codec: string;
					container: {
						kind: "unknown";
						raw: {
							[key: string]: unknown;
							kind: string;
						};
					} | {
						kind: "legacy";
					} | {
						init: string;
						kind: "cmaf";
					} | {
						kind: "loc";
					};
					delay?: number & $brand<"u53">;
					description?: string;
					enabled?: boolean;
					epoch?: Hang.Catalog.ReferenceEpoch;
					hls?: {
						duration: number & $brand<"u53">;
						track: string;
					};
					jitter?: number & $brand<"u53">;
					label?: string;
					layout?: string;
					numberOfChannels: number & $brand<"u53">;
					sampleRate: number & $brand<"u53">;
					track?: string;
					warmup?: number & $brand<"u53">;
				};
			};
		} | {
			renditions: Record<string, {
				bitrate?: number & $brand<"u53">;
				broadcast?: Hang.Catalog.RelativeBroadcast;
				codec: string;
				container: {
					kind: "unknown";
					raw: {
						[key: string]: unknown;
						kind: string;
					};
				} | {
					kind: "legacy";
				} | {
					init: string;
					kind: "cmaf";
				} | {
					kind: "loc";
				};
				delay?: number & $brand<"u53">;
				description?: string;
				enabled?: boolean;
				epoch?: Hang.Catalog.ReferenceEpoch;
				hls?: {
					duration: number & $brand<"u53">;
					track: string;
				};
				jitter?: number & $brand<"u53">;
				label?: string;
				layout?: string;
				numberOfChannels: number & $brand<"u53">;
				sampleRate: number & $brand<"u53">;
				track?: string;
				warmup?: number & $brand<"u53">;
			}>;
		};
		binary?: {
			tracks: Record<string, {
				[key: string]: unknown;
				bitrate?: number & $brand<"u53">;
				broadcast?: Hang.Catalog.RelativeBroadcast;
				compression?: string;
				delay?: number & $brand<"u53">;
				epoch?: Hang.Catalog.ReferenceEpoch;
				jitter?: number & $brand<"u53">;
				mime?: string;
				mode: string;
				track?: string;
			}>;
		};
		clock?: {
			timescale: number;
			wall: number & $brand<"u53">;
		};
		echo?: {
			path: Hang.Catalog.RelativeBroadcast;
		};
		json?: {
			tracks: Record<string, {
				[key: string]: unknown;
				bitrate?: number & $brand<"u53">;
				broadcast?: Hang.Catalog.RelativeBroadcast;
				compression?: string;
				delay?: number & $brand<"u53">;
				epoch?: Hang.Catalog.ReferenceEpoch;
				jitter?: number & $brand<"u53">;
				mode: string;
				schema?: string;
				track?: string;
			}>;
		};
		stats?: {
			track: string;
		};
		text?: {
			renditions: Record<string, {
				broadcast?: Hang.Catalog.RelativeBroadcast;
				container: {
					kind: "unknown";
					raw: {
						[key: string]: unknown;
						kind: string;
					};
				} | {
					kind: "legacy";
				} | {
					init: string;
					kind: "cmaf";
				} | {
					kind: "loc";
				};
				delay?: number & $brand<"u53">;
				epoch?: Hang.Catalog.ReferenceEpoch;
				format: string;
				jitter?: number & $brand<"u53">;
				label?: string;
				lang?: string;
				role: string;
				track?: string;
			}>;
		};
		version?: number;
		video?: {
			display: {
				height: number & $brand<"u53">;
				width: number & $brand<"u53">;
			} | undefined;
			flip: undefined;
			renditions: {
				[k: string]: {
					bitrate?: number & $brand<"u53">;
					broadcast?: Hang.Catalog.RelativeBroadcast;
					codec: string;
					codedHeight?: number & $brand<"u53">;
					codedWidth?: number & $brand<"u53">;
					color?: {
						contentLight?: {
							maxCll: number;
							maxFall: number;
						};
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
						};
						matrix?: number & $brand<"u8">;
						primaries?: number & $brand<"u8">;
						range?: "limited" | "full";
						transfer?: number & $brand<"u8">;
					};
					container: {
						kind: "unknown";
						raw: {
							[key: string]: unknown;
							kind: string;
						};
					} | {
						kind: "legacy";
					} | {
						init: string;
						kind: "cmaf";
					} | {
						kind: "loc";
					};
					delay?: number & $brand<"u53">;
					description?: string;
					displayAspectHeight?: number & $brand<"u53">;
					displayAspectWidth?: number & $brand<"u53">;
					enabled?: boolean;
					epoch?: Hang.Catalog.ReferenceEpoch;
					framerate?: number;
					hls?: {
						duration: number & $brand<"u53">;
						track: string;
					};
					interlaced?: boolean;
					jitter?: number & $brand<"u53">;
					label?: string;
					optimizeForLatency?: boolean;
					preference?: number;
					projection?: {
						kind: "unknown";
						raw: {
							[key: string]: unknown;
							kind: string;
						};
					} | {
						kind: "rectilinear";
					} | {
						fov?: number;
						kind: "equirectangular";
					};
					reorder?: number & $brand<"u8">;
					stereo?: string;
					track?: string;
					warmup?: number & $brand<"u53">;
				};
			};
			rotation: undefined;
		} | {
			display?: {
				height: number & $brand<"u53">;
				width: number & $brand<"u53">;
			};
			flip?: boolean;
			renditions: Record<string, {
				bitrate?: number & $brand<"u53">;
				broadcast?: Hang.Catalog.RelativeBroadcast;
				codec: string;
				codedHeight?: number & $brand<"u53">;
				codedWidth?: number & $brand<"u53">;
				color?: {
					contentLight?: {
						maxCll: number;
						maxFall: number;
					};
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
					};
					matrix?: number & $brand<"u8">;
					primaries?: number & $brand<"u8">;
					range?: "limited" | "full";
					transfer?: number & $brand<"u8">;
				};
				container: {
					kind: "unknown";
					raw: {
						[key: string]: unknown;
						kind: string;
					};
				} | {
					kind: "legacy";
				} | {
					init: string;
					kind: "cmaf";
				} | {
					kind: "loc";
				};
				delay?: number & $brand<"u53">;
				description?: string;
				displayAspectHeight?: number & $brand<"u53">;
				displayAspectWidth?: number & $brand<"u53">;
				enabled?: boolean;
				epoch?: Hang.Catalog.ReferenceEpoch;
				framerate?: number;
				hls?: {
					duration: number & $brand<"u53">;
					track: string;
				};
				interlaced?: boolean;
				jitter?: number & $brand<"u53">;
				label?: string;
				optimizeForLatency?: boolean;
				preference?: number;
				projection?: {
					kind: "unknown";
					raw: {
						[key: string]: unknown;
						kind: string;
					};
				} | {
					kind: "rectilinear";
				} | {
					fov?: number;
					kind: "equirectangular";
				};
				reorder?: number & $brand<"u8">;
				stereo?: string;
				track?: string;
				warmup?: number & $brand<"u53">;
			}>;
			rotation?: number;
		};
	} | undefined;
	set catalog(value: {
		[key: string]: unknown;
		audio?: {
			renditions: {
				[key: string]: {
					bitrate?: number & $brand<"u53">;
					broadcast?: Hang.Catalog.RelativeBroadcast;
					codec: string;
					container: {
						kind: "unknown";
						raw: {
							[key: string]: unknown;
							kind: string;
						};
					} | {
						kind: "legacy";
					} | {
						init: string;
						kind: "cmaf";
					} | {
						kind: "loc";
					};
					delay?: number & $brand<"u53">;
					description?: string;
					enabled?: boolean;
					epoch?: Hang.Catalog.ReferenceEpoch;
					hls?: {
						duration: number & $brand<"u53">;
						track: string;
					};
					jitter?: number & $brand<"u53">;
					label?: string;
					layout?: string;
					numberOfChannels: number & $brand<"u53">;
					sampleRate: number & $brand<"u53">;
					track?: string;
					warmup?: number & $brand<"u53">;
				};
			};
		} | {
			renditions: Record<string, {
				bitrate?: number & $brand<"u53">;
				broadcast?: Hang.Catalog.RelativeBroadcast;
				codec: string;
				container: {
					kind: "unknown";
					raw: {
						[key: string]: unknown;
						kind: string;
					};
				} | {
					kind: "legacy";
				} | {
					init: string;
					kind: "cmaf";
				} | {
					kind: "loc";
				};
				delay?: number & $brand<"u53">;
				description?: string;
				enabled?: boolean;
				epoch?: Hang.Catalog.ReferenceEpoch;
				hls?: {
					duration: number & $brand<"u53">;
					track: string;
				};
				jitter?: number & $brand<"u53">;
				label?: string;
				layout?: string;
				numberOfChannels: number & $brand<"u53">;
				sampleRate: number & $brand<"u53">;
				track?: string;
				warmup?: number & $brand<"u53">;
			}>;
		};
		binary?: {
			tracks: Record<string, {
				[key: string]: unknown;
				bitrate?: number & $brand<"u53">;
				broadcast?: Hang.Catalog.RelativeBroadcast;
				compression?: string;
				delay?: number & $brand<"u53">;
				epoch?: Hang.Catalog.ReferenceEpoch;
				jitter?: number & $brand<"u53">;
				mime?: string;
				mode: string;
				track?: string;
			}>;
		};
		clock?: {
			timescale: number;
			wall: number & $brand<"u53">;
		};
		echo?: {
			path: Hang.Catalog.RelativeBroadcast;
		};
		json?: {
			tracks: Record<string, {
				[key: string]: unknown;
				bitrate?: number & $brand<"u53">;
				broadcast?: Hang.Catalog.RelativeBroadcast;
				compression?: string;
				delay?: number & $brand<"u53">;
				epoch?: Hang.Catalog.ReferenceEpoch;
				jitter?: number & $brand<"u53">;
				mode: string;
				schema?: string;
				track?: string;
			}>;
		};
		stats?: {
			track: string;
		};
		text?: {
			renditions: Record<string, {
				broadcast?: Hang.Catalog.RelativeBroadcast;
				container: {
					kind: "unknown";
					raw: {
						[key: string]: unknown;
						kind: string;
					};
				} | {
					kind: "legacy";
				} | {
					init: string;
					kind: "cmaf";
				} | {
					kind: "loc";
				};
				delay?: number & $brand<"u53">;
				epoch?: Hang.Catalog.ReferenceEpoch;
				format: string;
				jitter?: number & $brand<"u53">;
				label?: string;
				lang?: string;
				role: string;
				track?: string;
			}>;
		};
		version?: number;
		video?: {
			display: {
				height: number & $brand<"u53">;
				width: number & $brand<"u53">;
			} | undefined;
			flip: undefined;
			renditions: {
				[k: string]: {
					bitrate?: number & $brand<"u53">;
					broadcast?: Hang.Catalog.RelativeBroadcast;
					codec: string;
					codedHeight?: number & $brand<"u53">;
					codedWidth?: number & $brand<"u53">;
					color?: {
						contentLight?: {
							maxCll: number;
							maxFall: number;
						};
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
						};
						matrix?: number & $brand<"u8">;
						primaries?: number & $brand<"u8">;
						range?: "limited" | "full";
						transfer?: number & $brand<"u8">;
					};
					container: {
						kind: "unknown";
						raw: {
							[key: string]: unknown;
							kind: string;
						};
					} | {
						kind: "legacy";
					} | {
						init: string;
						kind: "cmaf";
					} | {
						kind: "loc";
					};
					delay?: number & $brand<"u53">;
					description?: string;
					displayAspectHeight?: number & $brand<"u53">;
					displayAspectWidth?: number & $brand<"u53">;
					enabled?: boolean;
					epoch?: Hang.Catalog.ReferenceEpoch;
					framerate?: number;
					hls?: {
						duration: number & $brand<"u53">;
						track: string;
					};
					interlaced?: boolean;
					jitter?: number & $brand<"u53">;
					label?: string;
					optimizeForLatency?: boolean;
					preference?: number;
					projection?: {
						kind: "unknown";
						raw: {
							[key: string]: unknown;
							kind: string;
						};
					} | {
						kind: "rectilinear";
					} | {
						fov?: number;
						kind: "equirectangular";
					};
					reorder?: number & $brand<"u8">;
					stereo?: string;
					track?: string;
					warmup?: number & $brand<"u53">;
				};
			};
			rotation: undefined;
		} | {
			display?: {
				height: number & $brand<"u53">;
				width: number & $brand<"u53">;
			};
			flip?: boolean;
			renditions: Record<string, {
				bitrate?: number & $brand<"u53">;
				broadcast?: Hang.Catalog.RelativeBroadcast;
				codec: string;
				codedHeight?: number & $brand<"u53">;
				codedWidth?: number & $brand<"u53">;
				color?: {
					contentLight?: {
						maxCll: number;
						maxFall: number;
					};
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
					};
					matrix?: number & $brand<"u8">;
					primaries?: number & $brand<"u8">;
					range?: "limited" | "full";
					transfer?: number & $brand<"u8">;
				};
				container: {
					kind: "unknown";
					raw: {
						[key: string]: unknown;
						kind: string;
					};
				} | {
					kind: "legacy";
				} | {
					init: string;
					kind: "cmaf";
				} | {
					kind: "loc";
				};
				delay?: number & $brand<"u53">;
				description?: string;
				displayAspectHeight?: number & $brand<"u53">;
				displayAspectWidth?: number & $brand<"u53">;
				enabled?: boolean;
				epoch?: Hang.Catalog.ReferenceEpoch;
				framerate?: number;
				hls?: {
					duration: number & $brand<"u53">;
					track: string;
				};
				interlaced?: boolean;
				jitter?: number & $brand<"u53">;
				label?: string;
				optimizeForLatency?: boolean;
				preference?: number;
				projection?: {
					kind: "unknown";
					raw: {
						[key: string]: unknown;
						kind: string;
					};
				} | {
					kind: "rectilinear";
				} | {
					fov?: number;
					kind: "equirectangular";
				};
				reorder?: number & $brand<"u8">;
				stereo?: string;
				track?: string;
				warmup?: number & $brand<"u53">;
			}>;
			rotation?: number;
		};
	} | undefined);
	get catalogFormat(): "hang" | "msf" | "hangz" | "manual" | undefined;
	set catalogFormat(value: "hang" | "msf" | "hangz" | "manual" | undefined);
	connectedCallback(): void;
	connection: Net.Connection;
	readonly controls: {
		audio: {
			muted: Signals.Signal<boolean>;
			target: Signals.Signal<Audio.Target | undefined>;
			volume: Signals.Signal<number>;
		};
		buffer: Signals.Signal<Clock.Milli>;
		delay: Signals.Signal<Delay>;
		paused: Signals.Signal<boolean>;
		text: {
			target: Signals.Signal<string | undefined>;
		};
		video: {
			backend: Signals.Signal<Video.Backend>;
			target: Signals.Signal<Video.Target | undefined>;
			visible: Signals.Signal<Video.Visible>;
		};
	};
	get delay(): Delay;
	set delay(value: Delay);
	disconnectedCallback(): void;
	emitter: Audio.Emitter;
	get muted(): boolean;
	set muted(value: boolean);
	get name(): Net.Path.Valid;
	set name(value: string | Net.Path.Valid);
	static observedAttributes: readonly ["url", "versions", "name", "paused", "volume", "muted", "visible", "backend", "announced", "delay", "buffer", "catalog-format", "captions"];
	get paused(): boolean;
	set paused(value: boolean);
	readonly player: Player;
	readonly renderer: Video.Renderer;
	reset(): void;
	readonly signals: Signals.Effect;
	readonly sync: Sync;
	text: Text.Source;
	textRenderer: Text.Renderer;
	get url(): URL | undefined;
	set url(value: string | URL | undefined);
	get versions(): readonly string[] | undefined;
	set versions(value: readonly string[] | undefined);
	readonly video: Video.Decoder;
	get visible(): Video.Visible;
	set visible(value: Video.Visible);
	get volume(): number;
	set volume(value: number);
}

export function parseCatalogFormat(value: string | null): "hang" | "msf" | "hangz" | "manual" | undefined;
```
