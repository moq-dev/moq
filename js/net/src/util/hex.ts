export function toBytes(hex: string): Uint8Array<ArrayBuffer> {
	hex = hex.startsWith("0x") ? hex.slice(2) : hex;
	if (hex.length % 2) {
		throw new Error("invalid hex string length");
	}

	const matches = hex.match(/.{2}/g);
	if (!matches) {
		throw new Error("invalid hex string format");
	}

	return new Uint8Array(matches.map((byte) => parseInt(byte, 16)));
}

export function fromBytes(bytes: Uint8Array): string {
	return Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0")).join("");
}
