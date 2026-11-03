/**
 * Reduce and compare a peer grant shaped against the union index, swept over its encoded size
 * up to a whole AUTH_OK. Mirrors the Shape cases of the Rust `pattern-algebra` benchmark.
 */
import { Pattern, Patterns } from "@moq/pattern";

/** The largest moq-lite control message, and so the largest AUTH_OK. */
const MAX_MESSAGE = 65535;

/** Positions a mask chooses `*` or `y` for, half of them `*`. */
const FREE = 14;

/** A varint's size as AUTH_OK writes counts and lengths; QUIC's, never smaller than lite's. */
function varint(value: number): number {
	return value < 64 ? 1 : value < 16384 ? 2 : 4;
}

/** A union's encoded size as AUTH_OK carries it: a count, then each text with its length. */
function wire(texts: string[]): number {
	return texts.reduce((size, text) => size + varint(text.length) + text.length, varint(texts.length));
}

/**
 * The grant's texts, filling at most `bytes` encoded, and a previous grant covering every one,
 * as a regrant compares them. Every member is incomparable, so reduction keeps them all.
 *
 * `mask` pairs `*`/`y` masks of equal weight, each ending in `y`, with literal `y/…/y/xNNNN`
 * queries that every mask matches but for the last segment. `chain` is the correlated partials
 * `a^i*z^(n-i)`, each sharing prefixes and suffixes with all the others.
 */
function grant(shape: "mask" | "chain", bytes: number): { texts: string[]; previous: Patterns } {
	const parse = (texts: string[]) => new Patterns(texts.map((text) => Pattern.parse(text)));
	if (shape === "chain") {
		const chain = (n: number) =>
			Array.from({ length: n - 1 }, (_, i) => `${"a".repeat(i + 1)}*${"z".repeat(n - i - 1)}`);
		let n = 2;
		while (wire(chain(n + 1)) <= bytes) n++;
		const texts = chain(n);
		return { texts, previous: parse(texts) };
	}
	const masks: string[] = [];
	const queries: string[] = [];
	let size = 0;
	for (let bits = 0, index = 0; bits < 1 << FREE; bits++) {
		let weight = 0;
		for (let at = 0; at < FREE; at++) weight += (bits >> at) & 1;
		if (weight !== FREE / 2) continue;
		const mask = [...Array.from({ length: FREE }, (_, at) => ((bits >> at) & 1 ? "*" : "y")), "y"].join("/");
		const query = `${Array(FREE).fill("y").join("/")}/x${String(index++).padStart(4, "0")}`;
		size += varint(mask.length) + mask.length + varint(query.length) + query.length;
		if (varint(masks.length * 2 + 2) + size > bytes) break;
		masks.push(mask);
		queries.push(query);
	}
	// `*/…/*/x*` holds every query, and a walk tries a literal before `*`, so each query
	// reaches it only after every mask.
	const previous = parse([...masks, `${Array(FREE).fill("*").join("/")}/x*`]);
	return { texts: [...masks, ...queries], previous };
}

/** Time each shape's reduction and regrant comparison, repeated to at least 100 ms per row. */
export function run(): void {
	console.log("bytes,shape,op,members,ms_per_op");
	for (const shape of ["mask", "chain"] as const) {
		for (const bytes of [4096, 16384, MAX_MESSAGE]) {
			const { texts, previous } = grant(shape, bytes);
			const size = wire(texts);
			if (size > bytes || size * 16 <= bytes * 15) throw new Error(`${shape} fills ${size} of ${bytes} bytes`);
			const message = texts.map((text) => Pattern.parse(text));
			const reduced = new Patterns(message);
			if (reduced.size !== message.length) throw new Error(`${shape} members are not incomparable`);
			if (!previous.covers(reduced)) throw new Error(`${shape} previous grant does not cover it`);
			const ops: [string, () => unknown][] = [
				["collect", () => new Patterns(message).size],
				["covers", () => previous.covers(reduced)],
			];
			for (const [op, body] of ops) {
				let iterations = 0;
				const start = performance.now();
				let elapsed = 0;
				while (elapsed < 100 || iterations < 3) {
					body();
					iterations++;
					elapsed = performance.now() - start;
				}
				console.log(`${bytes},${shape},${op},${message.length},${(elapsed / iterations).toFixed(3)}`);
			}
		}
	}
}

if (import.meta.main) run();
