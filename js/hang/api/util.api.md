# `@moq/hang/util`

```ts
import { Signals } from "@moq/hang";

export namespace Aac {
	export const SAMPLE_RATES: readonly number[];
	export function audioSpecificConfig(sampleRate: number, channelCount: number): Uint8Array;
	export function objectType(codec: string): 2 | 5 | 29 | undefined;
	export function pickRate(rate: number): number;
	export function supportsRate(rate: number): boolean;
}

export namespace Gesture {
	export function unlock(effect: Signals.Effect, context: AudioContext): Signals.Getter<boolean>;
}

export namespace Hacks {
	export const isChrome: boolean;
	export const isFirefox: boolean;
	export const isSafari: boolean;
}

export namespace Hex {
	export function fromBytes(bytes: Uint8Array): string;
	export function toBytes(hex: string): Uint8Array<ArrayBuffer>;
}

export namespace Libav {
	export function polyfill(): Promise<boolean>;
}

export namespace Opus {
	export const DEFAULT_SAMPLE_RATE: 48000;
	export const SAMPLE_RATES: readonly number[];
	export const WARMUP: 80;
	export function packetSamples(packet: Uint8Array): number | undefined;
	export function pickRate(rate: number): number;
	export function preSkip(description: Uint8Array): number;
	export function supportsRate(rate: number): boolean;
	export function toDOps(description: Uint8Array): Uint8Array;
}
```
