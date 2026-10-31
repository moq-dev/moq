// Every vector in drafts/moq-sign-00.json, run against the internal primitives.
import { expect, test } from "bun:test";
import { Epoch, type Path, Time } from "@moq/net";
import { Failure } from "./failure.ts";
import { PublicKey, SecretKey } from "./key.ts";
import { Scope } from "./scope.ts";

type Row = Record<string, unknown>;

const vectors = (await Bun.file(new URL("../../../drafts/moq-sign-00.json", import.meta.url)).json()) as {
	keys: Row[];
	frames: Row[];
	negative: Row[];
	invalid_ids: Row[];
};

function unhex(value: unknown): Uint8Array<ArrayBuffer> {
	return Uint8Array.from(String(value).match(/../g) ?? [], (byte) => Number.parseInt(byte, 16));
}

function hex(bytes: Uint8Array): string {
	return Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0")).join("");
}

// The vectors' path field is the raw signed text, including an empty one.
function scope(row: Row): Scope {
	return new Scope(String(row.path) as Path.Valid, Epoch.parse(String(row.epoch)), String(row.track));
}

// A row's timestamp at its timescale, or undefined when the timescale is 0 (untimed).
function timestamp(row: Row): Time.Timestamp | undefined {
	const scale = Number(row.timescale);
	return scale === 0 ? undefined : new Time.Timestamp(Number(row.timestamp), Time.Timescale(scale));
}

test("keys", async () => {
	for (const row of vectors.keys) {
		const endpoint = PublicKey.parse(String(row.endpoint));
		expect(endpoint.equals(PublicKey.fromBytes(unhex(row.public)))).toBe(true);
		expect(endpoint.toString()).toBe(String(row.endpoint));
		if (row.seed !== undefined) {
			expect((await SecretKey.fromSeed(unhex(row.seed))).public.equals(endpoint)).toBe(true);
		}
	}
});

test("frames", async () => {
	for (const row of vectors.frames) {
		const s = scope(row);
		const [group, frame, at] = [Number(row.group), Number(row.frame), timestamp(row)];
		expect(hex(s.message(group, frame, at, unhex(row.payload)))).toBe(String(row.message));
		const body = await s.sign(await SecretKey.fromSeed(unhex(row.seed)), group, frame, at, unhex(row.payload));
		expect(hex(body)).toBe(String(row.body));
		const payload = await s.verify(PublicKey.parse(String(row.endpoint)), group, frame, at, unhex(row.body));
		expect(hex(payload)).toBe(String(row.payload));
	}
});

test("negative", async () => {
	for (const row of vectors.negative) {
		const verified = scope(row).verify(
			PublicKey.parse(String(row.endpoint)),
			Number(row.group),
			Number(row.frame),
			timestamp(row),
			unhex(row.body),
		);
		await expect(verified).rejects.toEqual(new Failure("signature"));
	}
});

test("a fractional timestamp never verifies", async () => {
	const row = vectors.frames.find((frame) => frame.id === "timed");
	if (!row) throw new Error("expected the timed vector");
	const at = new Time.Timestamp(Number(row.timestamp) + 0.5, Time.Timescale(Number(row.timescale)));
	const verified = scope(row).verify(
		PublicKey.parse(String(row.endpoint)),
		Number(row.group),
		Number(row.frame),
		at,
		unhex(row.body),
	);
	await expect(verified).rejects.toEqual(new Failure("signature"));
});

test("invalid ids", () => {
	for (const row of vectors.invalid_ids) {
		expect(() => PublicKey.parse(String(row.text))).toThrow(new Failure("key"));
	}
	expect(() => PublicKey.fromBytes(new Uint8Array(31))).toThrow(new Failure("key"));
});
