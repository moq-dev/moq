export function toBytes(hex: string) {
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
