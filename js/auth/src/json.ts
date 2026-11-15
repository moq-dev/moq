// A JSON parser that refuses what `moq-auth`'s serde_json refuses, so a key loads in both
// languages or in neither.
//
// `JSON.parse` keeps a repeated member's last value, while serde refuses a field it reads
// twice and ignores the members it doesn't read. So a repeated member parses here as
// `REPEATED`, which every schema refuses where it reads the member and strips where it
// doesn't. Beyond that it refuses the inputs serde_json does and `JSON.parse` doesn't:
// nesting past serde_json's recursion limit, a lone surrogate, and a number too large for
// a double.

// What a member parses to when its object repeats it.
const REPEATED = Symbol("repeated member");

// serde_json refuses a document nesting arrays and objects 128 deep.
const MAX_DEPTH = 127;

const WHITESPACE = /[ \t\n\r]*/y;
const NUMBER = /-?(?:0|[1-9][0-9]*)(?:\.[0-9]+)?(?:[eE][+-]?[0-9]+)?/y;
// biome-ignore lint/suspicious/noControlCharactersInRegex: JSON refuses an unescaped control character in a string.
const PLAIN = /[^"\\\u0000-\u001f]*/y;
const LONE_SURROGATE = /\p{Surrogate}/u;
const ESCAPES: Record<string, string> = { '"': '"', "\\": "\\", "/": "/", b: "\b", f: "\f", n: "\n", r: "\r", t: "\t" };

/** Parse a JSON document as `moq-auth` does. Throws on anything serde_json refuses. */
export function parse(text: string): unknown {
	const parser = new Parser(text);
	const value = parser.value(0);
	parser.skip();
	if (parser.pos !== text.length) parser.fail("trailing characters");
	return value;
}

class Parser {
	readonly text: string;
	pos = 0;

	constructor(text: string) {
		this.text = text;
	}

	fail(message: string): never {
		throw new SyntaxError(`invalid JSON: ${message} at offset ${this.pos}`);
	}

	skip(): void {
		this.pos += this.match(WHITESPACE).length;
	}

	match(pattern: RegExp): string {
		pattern.lastIndex = this.pos;
		return pattern.exec(this.text)?.[0] ?? "";
	}

	eat(char: string): boolean {
		this.skip();
		if (this.text[this.pos] !== char) return false;
		this.pos++;
		return true;
	}

	value(depth: number): unknown {
		this.skip();
		const char = this.text[this.pos];
		if (char === "{" || char === "[") {
			if (depth === MAX_DEPTH) this.fail("recursion limit exceeded");
			this.pos++;
			return char === "{" ? this.object(depth + 1) : this.array(depth + 1);
		}
		if (char === '"') return this.string();
		for (const [word, value] of [
			["true", true],
			["false", false],
			["null", null],
		] as const) {
			if (this.text.startsWith(word, this.pos)) {
				this.pos += word.length;
				return value;
			}
		}
		const number = this.match(NUMBER);
		if (!number) this.fail("expected value");
		this.pos += number.length;
		const value = Number(number);
		if (!Number.isFinite(value)) this.fail("number out of range");
		return value;
	}

	object(depth: number): Record<string, unknown> {
		const object: Record<string, unknown> = {};
		if (this.eat("}")) return object;
		do {
			this.skip();
			if (this.text[this.pos] !== '"') this.fail("expected member name");
			const key = this.string();
			if (!this.eat(":")) this.fail("expected `:`");
			const value = this.value(depth);
			// Defined rather than assigned, so a `__proto__` member stays a plain member.
			Object.defineProperty(object, key, {
				value: Object.hasOwn(object, key) ? REPEATED : value,
				enumerable: true,
				writable: true,
				configurable: true,
			});
		} while (this.eat(","));
		if (!this.eat("}")) this.fail("expected `,` or `}`");
		return object;
	}

	array(depth: number): unknown[] {
		const array: unknown[] = [];
		if (this.eat("]")) return array;
		do array.push(this.value(depth));
		while (this.eat(","));
		if (!this.eat("]")) this.fail("expected `,` or `]`");
		return array;
	}

	string(): string {
		this.pos++; // the opening quote
		let out = "";
		for (;;) {
			const plain = this.match(PLAIN);
			out += plain;
			this.pos += plain.length;
			const char = this.text[this.pos++];
			if (char === '"') break;
			if (char === undefined) this.fail("unterminated string");
			if (char !== "\\") this.fail("control character in string");
			const escaped = this.text[this.pos++];
			if (escaped === "u") {
				const hex = this.text.slice(this.pos, this.pos + 4);
				if (!/^[0-9a-fA-F]{4}$/.test(hex)) this.fail("invalid unicode escape");
				out += String.fromCharCode(Number.parseInt(hex, 16));
				this.pos += 4;
			} else {
				const decoded = escaped === undefined ? undefined : ESCAPES[escaped];
				if (decoded === undefined) this.fail("invalid escape");
				out += decoded;
			}
		}
		if (LONE_SURROGATE.test(out)) this.fail("lone surrogate");
		return out;
	}
}
