/** Packaging-independent workloads and exact integer fixtures. */
export interface Workload {
	id: string;
	url: string;
	version: string;
	publishers: number;
	subscribers: number;
	frames: number;
	size: number;
	sequence: number;
}

export interface Sample extends Workload {
	backend: "js" | "wasm";
	transport: "webtransport" | "websocket";
	module: string;
	grow: boolean;
}

export const integers = [
	"0",
	"4294967295",
	"4294967296",
	"9007199254740991",
	"9007199254740992",
	"4611686018427387903",
	"4611686018427387904",
	"18446744073709551615",
];

export function payload(size: number, frame: number): Uint8Array {
	const value = new Uint8Array(size);
	for (let i = 0; i < size; i++) value[i] = (i + frame) % 251;
	return value;
}

export function verify(actual: Uint8Array, size: number, frame: number): void {
	if (actual.byteLength !== size) throw new Error(`frame ${frame}: ${actual.byteLength} bytes, expected ${size}`);
	for (let i = 0; i < size; i++) {
		if (actual[i] !== (i + frame) % 251) throw new Error(`frame ${frame}: incorrect byte ${i}`);
	}
}
