import { expect, test } from "bun:test";
import { ProtocolViolation } from "../error.ts";
import * as Path from "../path.ts";
import { Reader, Writer } from "../stream.ts";
import { decode, decodeTrack, encode, encodeTrack, fromTuple, reserved } from "./namespace.ts";
import { Version } from "./version.ts";

test("reserved namespaces preserve tuple fields and draft boundaries", () => {
	for (const [fields, expected] of [
		[["."], true],
		[[".", "child"], true],
		[[".session"], true],
		[[".session", "extension"], true],
		[[".session/extension"], false],
		[[".application"], false],
		[[".sessionish"], false],
	] as [string[], boolean][]) {
		const path = fromTuple(fields);
		expect(reserved(path, Version.DRAFT_22)).toBe(expected);
		expect(reserved(path, Version.DRAFT_18)).toBe(false);
	}
});

test("separators and escapes count toward a path but not its tuple fields", async () => {
	const segments = Array(Path.MAX_PARTS - 1)
		.fill("a")
		.join("/");
	const deepest = Path.from(`${segments}/${"é".repeat(Path.MAX_BYTES / 2 - 31)}`);
	expect(await roundTrip((w) => encode(w, deepest), decode)).toBe(deepest);

	expect(fromTuple([`${"/".repeat(Path.MAX_BYTES / 2 - 1)}a`]).length).toBe(Path.MAX_BYTES - 1);
	expect(() => fromTuple([`${"/".repeat(Path.MAX_BYTES / 2)}a`])).toThrow(ProtocolViolation);
});

test("a full track name counts field bytes and is refused before writing", async () => {
	// 4095 path bytes holding 2048 field bytes.
	const namespace = Path.from(`${"\\/".repeat(Path.MAX_BYTES / 2 - 1)}a`);
	const name = "x".repeat(2048);
	expect(await roundTrip((w) => encodeTrack(w, namespace, name), decodeTrack)).toEqual({ namespace, name });
	await expect(roundTrip((w) => encodeTrack(w, namespace, `${name}x`), decodeTrack)).rejects.toThrow(
		"full track name exceeds",
	);
});

test("every path, `\\` and `/` included, returns from its namespace tuple unchanged", async () => {
	let texts = [""];
	for (let length = 1; length <= 8; length++) {
		texts = texts.flatMap((text) => ["a", "/", "\\"].map((c) => text + c));
		for (const text of texts) {
			let path: Path.Valid;
			try {
				path = Path.from(text);
			} catch {
				continue;
			}
			expect(await roundTrip((w) => encode(w, path), decode)).toBe(path);
		}
	}
});

test("a lone backslash in a field is escaped on the way in", () => {
	expect(fromTuple(["foo\\bar/baz", "qux"])).toBe(Path.from("foo\\\\bar\\/baz/qux"));
});

/** Write with `write`, then read the bytes back with `read`. */
async function roundTrip<T>(write: (w: Writer) => Promise<void>, read: (r: Reader) => Promise<T>): Promise<T> {
	const bytes: number[] = [];
	const writer = new Writer(
		new WritableStream<Uint8Array>({
			write: (chunk) => {
				bytes.push(...chunk);
			},
		}),
		Version.DRAFT_22,
	);
	try {
		await write(writer);
	} finally {
		await writer.close();
	}
	return read(new Reader(undefined, Uint8Array.from(bytes), Version.DRAFT_22));
}
