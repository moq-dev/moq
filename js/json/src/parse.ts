import { DuplicateKey } from "./error.ts";

const decoder = new TextDecoder();

/**
 * Parse one JSON frame, refusing any object that repeats a key.
 *
 * `JSON.parse` keeps the last of a repeated key while other parsers keep the first or refuse, so
 * such a frame could mean two things to two readers. A full parser that reports duplicates costs
 * an order of magnitude more than `JSON.parse`, so this counts instead: once parsing has proven the
 * text valid, every colon outside a string separates a member's key from its value, and any member
 * the parsed value is missing was a repeat.
 */
export function parse(bytes: Uint8Array): unknown {
	const value: unknown = JSON.parse(decoder.decode(bytes));
	if (colons(bytes) !== members(value)) throw new DuplicateKey();
	return value;
}

// Count the colons outside strings. UTF-8 continuation bytes never collide with ASCII.
function colons(bytes: Uint8Array): number {
	let count = 0;
	for (let i = 0; i < bytes.length; i++) {
		const byte = bytes[i];
		if (byte === 0x22) {
			// Skip to the closing quote, stepping over each escaped byte.
			for (i++; bytes[i] !== 0x22; i++) {
				if (bytes[i] === 0x5c) i++;
			}
		} else if (byte === 0x3a) {
			count++;
		}
	}
	return count;
}

// Count the object members in a parsed value, without recursion so any nesting `JSON.parse` accepts
// cannot overflow the call stack.
function members(root: unknown): number {
	let count = 0;
	const pending = [root];
	while (pending.length > 0) {
		const value = pending.pop();
		if (typeof value !== "object" || value === null) continue;
		const items = Array.isArray(value) ? value : Object.values(value);
		if (!Array.isArray(value)) count += items.length;
		for (const item of items) pending.push(item);
	}
	return count;
}
