import { DuplicateKey } from "./error.ts";

const decoder = new TextDecoder("utf-8", { fatal: true, ignoreBOM: true });

/**
 * Parse one JSON frame, refusing malformed Unicode and any object that repeats a key.
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
		if (typeof value === "string") {
			if (!value.isWellFormed()) throw new SyntaxError("unpaired surrogate in JSON string");
		} else if (Array.isArray(value)) {
			for (const item of value) pending.push(item);
		} else if (typeof value === "object" && value !== null) {
			for (const [key, item] of Object.entries(value)) {
				if (!key.isWellFormed()) throw new SyntaxError("unpaired surrogate in JSON object key");
				count++;
				pending.push(item);
			}
		}
	}
	return count;
}
