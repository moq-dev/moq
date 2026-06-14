// Wire format for a set track. Each group is self-contained: frame 0 is a snapshot of every item,
// and each following frame is a single insert/remove delta.
//
// - snapshot: u32(count) followed by `count` repetitions of u32(len) then `len` item bytes.
// - delta: a one-byte op ('+' insert, '-' remove) followed by the item bytes to the end of frame.
//
// Lengths are big-endian u32 (not QUIC varints) so the format stays self-contained and trivially
// matches the Rust implementation (`moq-data`).

export const INSERT = 0x2b; // '+'
export const REMOVE = 0x2d; // '-'

/** A stable map key for an item's encoded bytes, giving the set value (not reference) semantics. */
export function keyOf(bytes: Uint8Array): string {
	let key = "";
	for (let i = 0; i < bytes.length; i++) key += String.fromCharCode(bytes[i]);
	return key;
}

export function encodeSnapshot(items: Uint8Array[]): Uint8Array {
	let total = 4;
	for (const item of items) total += 4 + item.length;

	const out = new Uint8Array(total);
	const view = new DataView(out.buffer);
	view.setUint32(0, items.length);

	let offset = 4;
	for (const item of items) {
		view.setUint32(offset, item.length);
		offset += 4;
		out.set(item, offset);
		offset += item.length;
	}
	return out;
}

export function decodeSnapshot(frame: Uint8Array): Uint8Array[] {
	if (frame.length < 4) throw new Error("snapshot is missing its count");
	const view = new DataView(frame.buffer, frame.byteOffset, frame.byteLength);
	const count = view.getUint32(0);

	const items: Uint8Array[] = [];
	let offset = 4;
	for (let i = 0; i < count; i++) {
		if (offset + 4 > frame.length) throw new Error("snapshot is missing an item length");
		const len = view.getUint32(offset);
		offset += 4;

		if (offset + len > frame.length) throw new Error("snapshot item runs past end of frame");
		items.push(frame.subarray(offset, offset + len));
		offset += len;
	}

	if (offset !== frame.length) throw new Error("snapshot has trailing bytes");
	return items;
}

export function encodeDelta(op: number, item: Uint8Array): Uint8Array {
	const out = new Uint8Array(1 + item.length);
	out[0] = op;
	out.set(item, 1);
	return out;
}

export function decodeDelta(frame: Uint8Array): [number, Uint8Array] {
	if (frame.length === 0) throw new Error("empty delta frame");
	return [frame[0], frame.subarray(1)];
}
