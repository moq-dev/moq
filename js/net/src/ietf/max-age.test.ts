import { expect, test } from "bun:test";
import { Reader, Writer } from "../stream.ts";
import { SubscribeOk } from "./subscribe.ts";
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

for (const version of Object.values(Version).filter(
	(version): version is IetfVersion => version !== Version.DRAFT_07,
)) {
	test(`MAX_CACHE_DURATION is optional and legacy receive-only on ${version}`, async () => {
		const legacy = version === Version.DRAFT_14 || version === Version.DRAFT_15 || version === Version.DRAFT_16;
		for (const maxCacheDuration of [undefined, 0n, 30_000n]) {
			const ok = new SubscribeOk({
				requestId: legacy ? 0n : undefined,
				trackAlias: 0n,
				properties: { maxCacheDuration },
			});
			const encoded = await bytes(version, (writer) => ok.encode(writer, version));
			const decoded = await SubscribeOk.decode(new Reader(undefined, encoded, version), version);
			expect(decoded.properties.maxCacheDuration).toBe(legacy ? undefined : maxCacheDuration);
		}
	});

	test(`MAX_CACHE_DURATION decodes from the right field on ${version}`, async () => {
		for (const age of [0n, 30_000n]) {
			const payload = await bytes(version, async (writer) => {
				const fields =
					version === Version.DRAFT_14
						? [0, 0, 0, 1, 0, 1, 4]
						: version === Version.DRAFT_15
							? [0, 0, 1, 4]
							: version === Version.DRAFT_16
								? [0, 0, 0, 4]
								: [0, 0, 4];
				for (const field of fields) await writer.u53(field);
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
