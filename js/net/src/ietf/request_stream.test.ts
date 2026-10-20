import { expect, test } from "bun:test";
import { ProtocolViolation } from "../error.ts";
import { Reader, Stream } from "../stream.ts";
import * as Varint from "../varint.ts";
import { type RequestKind, toRequestCode } from "./error.ts";
import { FetchError } from "./fetch.ts";
import { PublishError } from "./publish.ts";
import { PublishNamespaceError } from "./publish_namespace.ts";
import { RequestError } from "./request.ts";
import { namespaceUpdate, nextMessage, refuse, refuseUpdate } from "./request_stream.ts";
import { SubscribeError } from "./subscribe.ts";
import { SubscribeNamespaceError } from "./subscribe_namespace.ts";
import { TRACK_STATUS_ERROR_14 } from "./track.ts";
import { type IetfVersion, Version, versionName } from "./version.ts";

test("an oversized or truncated namespace update type is a protocol violation", async () => {
	// Both drafts use leading-ones varints.
	for (const version of [Version.DRAFT_18, Version.DRAFT_21]) {
		const oversized = Varint.encodeLeadingOnes(2n ** 53n);
		// A two-byte varint prefix with its second byte missing.
		const truncated = Uint8Array.of(0x80);
		for (const bytes of [oversized, truncated]) {
			const reader = new Reader(undefined, bytes, version);
			await expect(namespaceUpdate(reader, version, false)).rejects.toBeInstanceOf(ProtocolViolation);
		}
	}
});

test("namespace update framing refuses duplicate prefix parameters", async () => {
	for (const version of [Version.DRAFT_18, Version.DRAFT_21]) {
		const body = [2, 2, 0x34, 1, 1, 97, 0, 1, 1, 98];
		const reader = new Reader(undefined, Uint8Array.from([2, 0, body.length, ...body]), version);
		await expect(namespaceUpdate(reader, version, false)).rejects.toBeInstanceOf(ProtocolViolation);
	}
});

test("a repeated namespace update token is a request refusal", async () => {
	for (const version of [Version.DRAFT_18, Version.DRAFT_21]) {
		const body = [2, 2, 3, 3, 3, 0, 170, 0, 3, 3, 0, 170];
		const reader = new Reader(undefined, Uint8Array.from([2, 0, body.length, ...body]), version);
		expect(await namespaceUpdate(reader, version, false)).toEqual({
			requestId: 2n,
			refused: "malformed_auth_token",
		});
	}
});

for (const version of [Version.DRAFT_18, Version.DRAFT_19, Version.DRAFT_21, Version.DRAFT_22]) {
	test(`${versionName(version)} keeps GOAWAY state across request response phases`, async () => {
		// GOAWAY, an empty REQUEST_OK, and another GOAWAY on the same stream.
		const reader = new Reader(undefined, Uint8Array.of(0x10, 0, 2, 0, 0, 7, 0, 0, 0x10, 0, 2, 0, 0), version);
		expect(await nextMessage(reader, version, true)).toBe(7);
		expect(await reader.u16()).toBe(0);
		await expect(nextMessage(reader, version, true)).rejects.toBeInstanceOf(ProtocolViolation);
	});

	test(`${versionName(version)} keeps separate request streams' GOAWAY state`, async () => {
		for (let n = 0; n < 2; n++) {
			const reader = new Reader(undefined, Uint8Array.of(0x10, 0, 2, 0, 0, 7, 0, 0), version);
			expect(await nextMessage(reader, version, false)).toBe(7);
		}
	});
}

test("request message parsing preserves a transport session failure", async () => {
	const failed = Object.assign(new Error("session closed"), { source: "session" });
	const input = new ReadableStream<Uint8Array>({
		start(controller) {
			controller.error(failed);
		},
	});
	const reader = new Reader(input, undefined, Version.DRAFT_22);
	await expect(nextMessage(reader, Version.DRAFT_22, true)).rejects.toBe(failed);
});

/** A stream recording what is written to it. */
function recording(version: IetfVersion) {
	const written: number[] = [];
	const stream = new Stream({
		readable: new ReadableStream<Uint8Array>(),
		writable: new WritableStream<Uint8Array>({
			write: (chunk) => {
				written.push(...chunk);
			},
		}),
		version,
	});
	return { stream, wire: () => new Reader(undefined, Uint8Array.from(written), version) };
}

const refusal = { requestId: 7n, condition: "not_supported", reasonPhrase: "no" } as const;

test("draft 14 refuses each request kind with its own error message and REQUEST_ERROR's body", async () => {
	const ids: Record<RequestKind, number> = {
		subscribe: SubscribeError.id,
		fetch: FetchError.id,
		publish: PublishError.id,
		publish_namespace: PublishNamespaceError.id,
		subscribe_namespace: SubscribeNamespaceError.id,
		track_status: TRACK_STATUS_ERROR_14,
	};
	for (const [kind, id] of Object.entries(ids) as [RequestKind, number][]) {
		const { stream, wire } = recording(Version.DRAFT_14);
		await refuse(stream, Version.DRAFT_14, kind, refusal);
		const r = wire();
		expect(await r.u53()).toBe(id);
		const err = await RequestError.decode(r, Version.DRAFT_14);
		expect(err.requestId).toBe(7n);
		expect(err.errorCode).toBe(toRequestCode("not_supported", kind, Version.DRAFT_14));
		expect(await r.done()).toBe(true);
	}
});

test("a refusal names its Request ID on drafts 15 and 16 only", async () => {
	for (const version of [Version.DRAFT_15, Version.DRAFT_16, Version.DRAFT_17, Version.DRAFT_21]) {
		const { stream, wire } = recording(version);
		await refuse(stream, version, "fetch", refusal);
		const r = wire();
		expect(await r.u53()).toBe(RequestError.id);
		const err = await RequestError.decode(r, version);
		expect(err.requestId).toBe(version <= Version.DRAFT_16 ? 7n : undefined);
		expect(err.reasonPhrase).toBe("no");
	}
});

test("draft 14 answers no update", async () => {
	for (const version of [Version.DRAFT_14, Version.DRAFT_15]) {
		const { stream, wire } = recording(version);
		await refuseUpdate(stream.writer, version, "subscribe", refusal);
		const r = wire();
		if (version === Version.DRAFT_14) {
			expect(await r.done()).toBe(true);
			continue;
		}
		expect(await r.u53()).toBe(RequestError.id);
		expect((await RequestError.decode(r, version)).requestId).toBe(7n);
	}
});
