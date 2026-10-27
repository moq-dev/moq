import { expect, test } from "bun:test";
import { Reader, Writer } from "../stream.ts";
import { decode, encodable, encode, fromTuple, reserved } from "./namespace.ts";
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

test("namespace encoding uses decoded UTF-8 field bounds", async () => {
	for (const [parts, fits] of [
		[["é".repeat(2048)], true],
		[[`${"é".repeat(2048)}x`], false],
		[["a".repeat(2048), "b".repeat(2048)], true],
		[["a".repeat(2048), "b".repeat(2049)], false],
		[[`${"/".repeat(4095)}a`], true],
		[["\\".repeat(4097)], false],
		[Array(32).fill("a"), true],
		[Array(33).fill("a"), false],
	] as [string[], boolean][]) {
		const namespace = fromTuple(parts);
		expect(encodable(namespace)).toBe(fits);
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
			if (fits) {
				await encode(writer, namespace);
				expect(await decode(new Reader(undefined, Uint8Array.from(bytes), Version.DRAFT_22))).toBe(namespace);
			} else {
				await expect(encode(writer, namespace)).rejects.toThrow("namespace exceeds");
				expect(bytes).toEqual([]);
			}
		} finally {
			await writer.close();
		}
	}
});
