import { DuplicateKey } from "./error.ts";

const decoder = new TextDecoder("utf-8", { fatal: true, ignoreBOM: true });

/**
 * Parse one JSON frame, refusing malformed Unicode, any object that repeats a key, nesting 128
 * containers deep, and a number outside the double range.
 *
 * `JSON.parse` keeps the last of a repeated key while other parsers keep the first or refuse, so
 * such a frame could mean two things to two readers. A full parser that reports duplicates costs
 * an order of magnitude more than `JSON.parse`, so this counts instead: once parsing has proven the
 * text valid, every colon outside a string separates a member's key from its value, and any member
 * the parsed value is missing was a repeat.
 */
export function parse(bytes: Uint8Array): unknown {
	const value: unknown = JSON.parse(decoder.decode(bytes));
	if (colons(bytes) !== check(value)) throw new DuplicateKey();
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

// Nesting at which serde_json refuses a document, so both implementations accept the same ones and
// the recursive merge, diff, clone and schema code never overflows the call stack.
const MAX_DEPTH = 128;

/**
 * Serialize a value into JSON that {@link parse} accepts once a frame wraps it in `envelope`
 * containers, refusing what it would refuse: an unpaired surrogate, or nesting too deep.
 *
 * `JSON.stringify` escapes an unpaired surrogate as valid JSON text that still decodes to one, and
 * never limits nesting, so its output alone would publish frames every reader refuses. Scanning the
 * text rather than the value sees exactly what `toJSON` and dropped fields leave on the wire.
 */
export function stringify(value: unknown, envelope = 0): string {
	const text: string | undefined = JSON.stringify(value);
	// A top-level undefined, function, or symbol would otherwise frame as empty bytes.
	if (text === undefined) throw new TypeError("value is not representable as JSON");

	// `JSON.stringify` keeps a surrogate pair literal and escapes only an unpaired surrogate, as a
	// lowercase `\ud800` through `\udfff`. Backslashes appear only inside strings, so each such
	// escape that an odd run of backslashes does not itself escape is one.
	for (let at = text.indexOf("\\ud"); at !== -1; at = text.indexOf("\\ud", at + 1)) {
		const high = text.charCodeAt(at + 3);
		if ((high === 0x38 || high === 0x39 || (high >= 0x61 && high <= 0x66)) && escaped(text, at + 1)) {
			throw new TypeError("unpaired surrogate in JSON string");
		}
	}

	// Each level takes an opening and a closing bracket, so a short text cannot pass the limit.
	const limit = MAX_DEPTH - 1 - envelope;
	if (text.length <= 2 * limit) return text;

	let depth = 0;
	for (let i = 0; i < text.length; i++) {
		const char = text.charCodeAt(i);
		if (char === 0x22) {
			// Step through a short string, which is cheaper than a native search, then search for the
			// closing quote: the first one no backslash escapes.
			const short = i + 32;
			for (i++; i < short && text.charCodeAt(i) !== 0x22; i++) {
				if (text.charCodeAt(i) === 0x5c) i++;
			}
			if (i >= short) {
				do i = text.indexOf('"', i);
				while (escaped(text, i++));
				i--;
			}
		} else if (char === 0x5b || char === 0x7b) {
			if (++depth > limit) throw new TypeError("JSON nested too deeply");
		} else if (char === 0x5d || char === 0x7d) {
			depth--;
		}
	}
	return text;
}

// Whether the character at `at` follows an odd run of backslashes, which escapes it.
function escaped(text: string, at: number): boolean {
	let run = 0;
	while (text.charCodeAt(at - 1 - run) === 0x5c) run++;
	return run % 2 === 1;
}

// Count the object members in a parsed value, refusing what Rust's moq-json refuses: nesting
// `MAX_DEPTH` containers deep, a number outside the double range, and an unpaired surrogate.
// Iterative so the walk itself cannot overflow the call stack before the depth check fires.
function check(root: unknown): number {
	let count = 0;
	const pending: [unknown, number][] = [[root, 0]];
	while (pending.length > 0) {
		const [value, depth] = pending.pop() as [unknown, number];
		if (typeof value === "string") {
			if (!value.isWellFormed()) throw new SyntaxError("unpaired surrogate in JSON string");
		} else if (typeof value === "number") {
			if (!Number.isFinite(value)) throw new SyntaxError("JSON number out of range");
		} else if (typeof value === "object" && value !== null) {
			if (depth + 1 >= MAX_DEPTH) throw new SyntaxError("JSON nested too deeply");
			if (Array.isArray(value)) {
				for (const item of value) pending.push([item, depth + 1]);
			} else {
				for (const [key, item] of Object.entries(value)) {
					if (!key.isWellFormed()) throw new SyntaxError("unpaired surrogate in JSON object key");
					count++;
					pending.push([item, depth + 1]);
				}
			}
		}
	}
	return count;
}
