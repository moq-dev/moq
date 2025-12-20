import type { Reader, Writer } from "../stream.ts";

export const Parameter = {
	MaxRequestId: 2n,
	Implementation: 7n,
} as const;

export class Parameters {
	vars: Map<bigint, bigint>;
	bytes: Map<bigint, Uint8Array>;

	constructor() {
		this.vars = new Map();
		this.bytes = new Map();
	}

	get size() {
		return this.vars.size + this.bytes.size;
	}

	setBytes(id: bigint, value: Uint8Array) {
		if (id % 2n !== 1n) {
			throw new Error(`invalid parameter id: ${id.toString()}, must be odd`);
		}
		this.bytes.set(id, value);
	}

	setVarint(id: bigint, value: bigint) {
		if (id % 2n !== 0n) {
			throw new Error(`invalid parameter id: ${id.toString()}, must be even`);
		}
		this.vars.set(id, value);
	}

	getBytes(id: bigint): Uint8Array | undefined {
		if (id % 2n !== 1n) {
			throw new Error(`invalid parameter id: ${id.toString()}, must be odd`);
		}
		return this.bytes.get(id);
	}

	getVarint(id: bigint): bigint | undefined {
		if (id % 2n !== 0n) {
			throw new Error(`invalid parameter id: ${id.toString()}, must be even`);
		}
		return this.vars.get(id);
	}

	removeBytes(id: bigint): boolean {
		if (id % 2n !== 1n) {
			throw new Error(`invalid parameter id: ${id.toString()}, must be odd`);
		}
		return this.bytes.delete(id);
	}

	removeVarint(id: bigint): boolean {
		if (id % 2n !== 0n) {
			throw new Error(`invalid parameter id: ${id.toString()}, must be even`);
		}
		return this.vars.delete(id);
	}

	async encode(w: Writer) {
		await w.u53(this.vars.size + this.bytes.size);
		for (const [id, value] of this.vars) {
			await w.u62(id);
			await w.u62(value);
		}

		for (const [id, value] of this.bytes) {
			await w.u62(id);
			await w.u53(value.length);
			await w.write(value);
		}
	}

	static async decode(r: Reader): Promise<Parameters> {
		const count = await r.u53();
		const params = new Parameters();

		for (let i = 0; i < count; i++) {
			const id = await r.u62();

			// Per draft-ietf-moq-transport-14 Section 1.4.2:
			// - If Type is even, Value is a single varint (no length prefix)
			// - If Type is odd, Value has a length prefix followed by bytes
			if (id % 2n === 0n) {
				if (params.vars.has(id)) {
					throw new Error(`duplicate parameter id: ${id.toString()}`);
				}

				// Even: read varint and store as encoded bytes
				const varint = await r.u62();
				params.setVarint(id, varint);
			} else {
				if (params.bytes.has(id)) {
					throw new Error(`duplicate parameter id: ${id.toString()}`);
				}

				// Odd: read length-prefixed bytes
				const size = await r.u53();
				const bytes = await r.read(size);
				params.setBytes(id, bytes);
			}
		}

		return params;
	}
}
