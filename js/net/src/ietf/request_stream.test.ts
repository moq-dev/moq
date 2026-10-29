import { expect, test } from "bun:test";
import { ProtocolViolation, SessionCode } from "../error.ts";
import { Reader, Stream } from "../stream.ts";
import * as Varint from "../varint.ts";
import { RequestError } from "./request.ts";
import { namespaceUpdate, nextMessage, RequestStream, refuse } from "./request_stream.ts";
import type { Session } from "./session.ts";
import { type IetfVersion, Version, versionName } from "./version.ts";

/**
 * A request stream the peer wrote `bytes` on, then optionally ended once they were read,
 * recording how the session was closed.
 */
function requested(version: IetfVersion, bytes: Uint8Array, end?: "fin" | "reset") {
	const closes: (WebTransportCloseInfo | undefined)[] = [];
	const quic = { close: (info?: WebTransportCloseInfo) => closes.push(info) } as unknown as WebTransport;
	const session = { version, client: true, close: () => closes.push(undefined) } as unknown as Session;
	const stream = new Stream({
		readable: new ReadableStream<Uint8Array>({
			start(c) {
				c.enqueue(bytes);
			},
			// Pulled once the bytes are read, whether or not anyone reads further.
			pull(c) {
				if (end === "fin") c.close();
				// Shaped like the WebTransportError a peer's RESET_STREAM fails the read with.
				if (end === "reset")
					c.error(Object.assign(new Error("reset"), { source: "stream", streamErrorCode: null }));
			},
		}),
		writable: new WritableStream<Uint8Array>(),
		version,
	});
	return { request: new RequestStream(stream, session, quic), closes };
}

// GOAWAY with no new session and no timeout.
const GOAWAY = [0x10, 0, 2, 0, 0];

test("an oversized or truncated request stream message type is a protocol violation", async () => {
	// Both drafts use leading-ones varints.
	for (const version of [Version.DRAFT_18, Version.DRAFT_20, Version.DRAFT_22]) {
		const oversized = Varint.encodeLeadingOnes(2n ** 53n);
		// A two-byte varint prefix with its second byte missing.
		const truncated = Uint8Array.of(0x80);
		for (const bytes of [oversized, truncated]) {
			const { request, closes } = requested(version, bytes, "fin");
			await expect(request.next()).rejects.toBeInstanceOf(ProtocolViolation);
			expect(closes[0]?.closeCode).toBe(SessionCode.ProtocolViolation);
		}
	}
});

test("namespace update framing refuses duplicate prefix parameters", async () => {
	for (const version of [Version.DRAFT_18, Version.DRAFT_20, Version.DRAFT_22]) {
		const body = [2, 2, 0x34, 1, 1, 97, 0, 1, 1, 98];
		const reader = new Reader(undefined, Uint8Array.from([0, body.length, ...body]), version);
		await expect(namespaceUpdate({ type: 2, reader }, version)).rejects.toBeInstanceOf(ProtocolViolation);
	}
});

test("a repeated namespace update token is a request refusal", async () => {
	for (const version of [Version.DRAFT_18, Version.DRAFT_20, Version.DRAFT_22]) {
		const body = [2, 2, 3, 3, 3, 0, 170, 0, 3, 3, 0, 170];
		const reader = new Reader(undefined, Uint8Array.from([0, body.length, ...body]), version);
		expect(await namespaceUpdate({ type: 2, reader }, version)).toEqual({
			requestId: 2n,
			refused: "malformed_auth_token",
		});
	}
});

for (const version of [Version.DRAFT_18, Version.DRAFT_20, Version.DRAFT_22]) {
	test(`${versionName(version)} a request nobody reads yet still refuses a second GOAWAY`, async () => {
		const { request, closes } = requested(version, Uint8Array.of(...GOAWAY, ...GOAWAY));
		await request.ended;
		expect(closes).toEqual([
			{ closeCode: SessionCode.ProtocolViolation, reason: "second GOAWAY on a request stream" },
			undefined,
		]);
		// Ending it again closes nothing more.
		request.abort(new ProtocolViolation("again"));
		request.close();
		expect(closes).toHaveLength(2);
	});

	test(`${versionName(version)} a request stream's messages are taken whole, around a GOAWAY`, async () => {
		// An empty REQUEST_OK, then FIN.
		const { request, closes } = requested(version, Uint8Array.of(...GOAWAY, 7, 0, 0), "fin");
		const framed = await request.next();
		expect(framed?.type).toBe(7);
		expect(await framed?.reader.u16()).toBe(0);
		expect(await request.next()).toBeUndefined();
		// FIN cancels a draft-18 request; a draft-22 one lasts until reset.
		const ended = await Promise.race([request.ended.then(() => true), Bun.sleep(10).then(() => false)]);
		expect(ended).toBe(version === Version.DRAFT_18);
		expect(closes).toEqual([]);
	});

	test(`${versionName(version)} a request ends behind a message nobody took`, async () => {
		// FIN cancels only a draft-18 request; a reset cancels on every draft.
		const ends = version === Version.DRAFT_18 ? (["fin", "reset"] as const) : (["reset"] as const);
		for (const end of ends) {
			// An empty REQUEST_UPDATE, then the peer's end.
			const { request, closes } = requested(version, Uint8Array.of(2, 0, 0), end);
			await request.ended;
			expect(closes).toEqual([]);
		}
	});
}

for (const version of [Version.DRAFT_18, Version.DRAFT_20, Version.DRAFT_22]) {
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

const refusal = { condition: "not_supported", reasonPhrase: "no" } as const;

test("a refusal carries its reason", async () => {
	for (const version of [Version.DRAFT_22]) {
		const { stream, wire } = recording(version);
		await refuse(stream, version, "fetch", refusal);
		const r = wire();
		expect(await r.u53()).toBe(RequestError.id);
		const err = await RequestError.decode(r, version);
		expect(err.reasonPhrase).toBe("no");
	}
});
