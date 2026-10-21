export class VarInt {
	static readonly MAX = (1n << 62n) - 1n;
	static readonly MAX_SIZE = 8;
	readonly value: bigint;

	constructor(value: bigint) {
		if (value < 0n || value > VarInt.MAX) {
			throw new Error(`VarInt value out of range: ${value}`);
		}
		this.value = value;
	}

	static from(value: number | bigint): VarInt {
		return new VarInt(BigInt(value));
	}

	size(): number {
		const x = this.value;
		if (x < 2n ** 6n) return 1;
		if (x < 2n ** 14n) return 2;
		if (x < 2n ** 30n) return 4;
		if (x < 2n ** 62n) return 8;
		throw new Error("VarInt value too large");
	}

	// Append to the provided buffer
	encode<T extends ArrayBuffer>(dst: Uint8Array<T>): Uint8Array<T> {
		const x = this.value;

		const size = this.size();
		if (dst.byteOffset + dst.byteLength + size > dst.buffer.byteLength) {
			throw new Error("destination buffer too small");
		}

		const view = new DataView(dst.buffer, dst.byteOffset + dst.byteLength, size);

		if (size === 1) {
			view.setUint8(0, Number(x));
		} else if (size === 2) {
			view.setUint16(0, (0b01 << 14) | Number(x), false);
		} else if (size === 4) {
			view.setUint32(0, (0b10 << 30) | Number(x), false);
		} else if (size === 8) {
			view.setBigUint64(0, (0b11n << 62n) | x, false);
		} else {
			throw new Error("VarInt value too large");
		}

		return new Uint8Array(dst.buffer, dst.byteOffset, dst.byteLength + size);
	}

	static decode(buffer: Uint8Array): [VarInt, Uint8Array] {
		if (buffer.byteLength < 1) {
			throw new Error("Unexpected end of buffer");
		}

		const view = new DataView(buffer.buffer, buffer.byteOffset);
		const firstByte = view.getUint8(0);
		const tag = firstByte >> 6;

		let value: bigint;
		let bytesRead: number;

		switch (tag) {
			case 0b00:
				value = BigInt(firstByte & 0b00111111);
				bytesRead = 1;
				break;
			case 0b01:
				if (2 > buffer.length) {
					throw new Error("Unexpected end of buffer");
				}
				value = BigInt(view.getUint16(0, false) & 0x3fff);
				bytesRead = 2;
				break;
			case 0b10:
				if (4 > buffer.length) {
					throw new Error("Unexpected end of buffer");
				}
				value = BigInt(view.getUint32(0, false) & 0x3fffffff);
				bytesRead = 4;
				break;
			case 0b11:
				if (8 > buffer.length) {
					throw new Error("Unexpected end of buffer");
				}
				value = view.getBigUint64(0, false) & 0x3fffffffffffffffn;
				bytesRead = 8;
				break;
			default:
				throw new Error("Invalid VarInt tag");
		}

		const remaining = new Uint8Array(buffer.buffer, buffer.byteOffset + bytesRead, buffer.byteLength - bytesRead);
		return [new VarInt(value), remaining];
	}
}
