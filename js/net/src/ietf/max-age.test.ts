import { expect, test } from "bun:test";
import * as Path from "../path.ts";
import { Reader, Writer } from "../stream.ts";
import { maxAgeFromSetup, maxAgeIntoSetup } from "./max_age.ts";
import { Parameters } from "./parameters.ts";
import { SetupOptions } from "./setup_options.ts";
import { Subscribe, SubscribeOk, SubscribeUpdate } from "./subscribe.ts";
import { type IetfVersion, Version } from "./version.ts";

async function bytes(version: IetfVersion, encode: (writer: Writer) => Promise<void>) {
	const chunks: Uint8Array[] = [];
	const writer = new Writer(
		new WritableStream<Uint8Array>({
			write: (chunk) => {
				chunks.push(chunk.slice());
			},
		}),
		version,
	);
	await encode(writer);
	writer.close();
	await writer.closed;
	return new Uint8Array(chunks.flatMap((chunk) => [...chunk]));
}

for (const version of Object.values(Version)) {
	test(`MAX_CACHE_DURATION is optional on ${version}`, async () => {
		for (const maxCacheDuration of [undefined, 0n, 30_000n]) {
			const ok = new SubscribeOk({ trackAlias: 0n, properties: { maxCacheDuration } });
			const encoded = await bytes(version, (writer) => ok.encode(writer, version));
			const decoded = await SubscribeOk.decode(new Reader(undefined, encoded, version), version);
			expect(decoded.properties.maxCacheDuration).toBe(maxCacheDuration);
		}
	});

	test(`MAX_CACHE_DURATION decodes from the right field on ${version}`, async () => {
		for (const age of [0n, 30_000n]) {
			const payload = await bytes(version, async (writer) => {
				for (const field of [0, 0, 4]) await writer.u53(field);
				await writer.u62(age);
			});
			const framed = await bytes(version, async (writer) => {
				await writer.u16(payload.length);
				await writer.write(payload);
			});
			const decoded = await SubscribeOk.decode(new Reader(undefined, framed, version), version);
			expect(decoded.properties.maxCacheDuration).toBe(age);
		}
	});
}

for (const version of Object.values(Version)) {
	test(`subscriber max age round trips on ${version}`, async () => {
		for (const maxAge of [undefined, 0, 250, Number.MAX_SAFE_INTEGER]) {
			const msg = new Subscribe({
				requestId: 0n,
				trackNamespace: Path.from("live"),
				trackName: "catalog",
				subscriberPriority: 128,
				maxAge,
			});
			const encoded = await bytes(version, (w) => msg.encode(w, version));
			expect((await Subscribe.decode(new Reader(undefined, encoded, version), version)).maxAge).toBe(maxAge);
			const parameters = new Parameters();
			parameters.maxAge = maxAge;
			const update = new SubscribeUpdate({ requestId: 2n, parameters });
			const encodedUpdate = await bytes(version, (w) => update.encode(w, version));
			expect(
				(await SubscribeUpdate.decode(new Reader(undefined, encodedUpdate, version), version)).parameters
					.maxAge,
			).toBe(maxAge);
		}
	});
}

test("subscriber max age has its own Setup Option and refuses unsafe budgets", () => {
	const params = new SetupOptions();
	expect(maxAgeFromSetup(params)).toBeFalse();
	maxAgeIntoSetup(params);
	expect(maxAgeFromSetup(params)).toBeTrue();
	const budget = new Parameters();
	for (const value of [-1, 0.5, Number.MAX_SAFE_INTEGER + 1])
		expect(() => {
			budget.maxAge = value;
		}).toThrow();
	budget.vars.set(0x915c6n, 1n << 53n);
	expect(() => budget.maxAge).toThrow();
});
