/** Decode a base64 string into bytes. Throws on invalid input. */
export function base64ToBytes(b64: string): Uint8Array {
	const raw = atob(b64);
	const bytes = new Uint8Array(raw.length);
	for (let i = 0; i < raw.length; i++) bytes[i] = raw.charCodeAt(i);
	return bytes;
}
