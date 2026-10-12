import { expect, test } from "bun:test";
import { SessionCode } from "../error.ts";
import * as Path from "../path.ts";
import { Reader, Stream, Writer } from "../stream.ts";
import { AuthError, AuthMessage, AuthOk, decodeAuthReplyMaybe, LiteAuthWire } from "./auth.ts";
import * as Lite from "./index.ts";
import * as Message from "./message.ts";

function patterns(...texts: string[]): Path.Patterns {
	return new Path.Patterns(texts.map((text) => Path.Pattern.parse(text)));
}

/** Round-trip bytes through a writer and back out of a reader. */
async function roundTrip(write: (w: Writer) => Promise<void>): Promise<Reader> {
	const chunks: Uint8Array[] = [];
	const writer = new Writer(
		new WritableStream({
			write(chunk) {
				chunks.push(new Uint8Array(chunk));
			},
		}),
		Lite.Version.DRAFT_07,
	);
	await write(writer);
	writer.close();
	await writer.closed;
	const size = chunks.reduce((n, c) => n + c.byteLength, 0);
	const buf = new Uint8Array(size);
	let offset = 0;
	for (const chunk of chunks) {
		buf.set(chunk, offset);
		offset += chunk.byteLength;
	}
	return new Reader(undefined, buf, Lite.Version.DRAFT_07);
}

test("AUTH round-trips its token", async () => {
	const r = await roundTrip((w) => new AuthMessage(new TextEncoder().encode("jwt")).encode(w, Lite.Version.DRAFT_07));
	const msg = await AuthMessage.decode(r, Lite.Version.DRAFT_07);
	expect(new TextDecoder().decode(msg.token)).toBe("jwt");
});

test("AUTH_OK and AUTH_ERROR round-trip behind their type", async () => {
	const ok = new AuthOk(patterns("**"), patterns(), 60_000);
	const err = new AuthError(SessionCode.Unauthorized, "expired");
	const r = await roundTrip(async (w) => {
		await ok.encode(w, Lite.Version.DRAFT_07);
		await err.encode(w, Lite.Version.DRAFT_07);
	});
	const first = await decodeAuthReplyMaybe(r, Lite.Version.DRAFT_07);
	expect(first).toBeInstanceOf(AuthOk);
	if (!(first instanceof AuthOk)) throw new Error("unreachable");
	// `**` grants everything; the empty list grants nothing.
	expect(first.publish.equals(new Path.Patterns([Path.Pattern.all()]))).toBe(true);
	expect(first.subscribe.size).toBe(0);
	expect(first.expires).toBe(60_000);

	const second = await decodeAuthReplyMaybe(r, Lite.Version.DRAFT_07);
	expect(second).toEqual(err);
	expect(await decodeAuthReplyMaybe(r, Lite.Version.DRAFT_07)).toBeUndefined();
});

test("literal and wildcard grants travel exactly, never widened", async () => {
	const ok = new AuthOk(patterns("room/alice", "room/*/cam", "**/demo.hang"), patterns("", "lobby/**"));
	const r = await roundTrip((w) => ok.encode(w, Lite.Version.DRAFT_07));
	const got = await decodeAuthReplyMaybe(r, Lite.Version.DRAFT_07);
	if (!(got instanceof AuthOk)) throw new Error("expected AUTH_OK");
	expect(got.publish.equals(ok.publish)).toBe(true);
	expect(got.subscribe.equals(ok.subscribe)).toBe(true);
});

// The same bytes as `auth_ok_golden` in `rs/moq-net/src/lite/auth.rs`.
test("AUTH_OK matches the Rust encoding", async () => {
	const ok = new AuthOk(patterns("room/*/cam", "**/b.hang"), patterns(""), 1000);
	const r = await roundTrip((w) => ok.encode(w, Lite.Version.DRAFT_07));
	const text = (s: string) => [s.length, ...new TextEncoder().encode(s)];
	expect([...(await r.readAll())]).toEqual([
		0x00, // AUTH_OK
		0x1a, // length
		0x02, // publish count, in canonical order
		...text("**/b.hang"),
		...text("room/*/cam"),
		0x01, // subscribe count
		0x00, // the empty pattern: the root alone
		0x83,
		0xe8, // expires: 1000ms, as a leading-ones varint
	]);
});

// Mirrors `oversized_message_is_refused` in `rs/moq-net/src/lite/auth.rs`: the peer would
// refuse it, so the acceptor withholds it, type included, before sending AUTH_ERROR.
test("a reply larger than the peer reads writes nothing", async () => {
	const body = new Uint8Array(Message.MAX_MESSAGE_SIZE + 1);
	const r = await roundTrip(async (w) => {
		await expect(Message.encode(w, (w) => w.write(body), { id: AuthOk.id })).rejects.toThrow("message too large");
	});
	expect((await r.readAll()).byteLength).toBe(0);
});

test("only valid, canonical patterns decode", async () => {
	for (const text of ["*/**", "/room", "room/", "room//a", "a*b*c", "**/**", "a**"]) {
		const r = await roundTrip(async (w) => {
			const body = new TextEncoder().encode(text);
			await w.u53(0); // AUTH_OK
			await w.u53(1 + 1 + body.byteLength + 1 + 1);
			await w.u53(1);
			await w.string(text);
			await w.u53(0);
			await w.u53(0);
		});
		await expect(decodeAuthReplyMaybe(r, Lite.Version.DRAFT_07)).rejects.toThrow();
	}
});

test.each([
	["lite-05", Lite.Version.DRAFT_05],
	["lite-06", Lite.Version.DRAFT_06],
])("%s carries no AUTH", async (_, version) => {
	await expect(roundTrip((w) => new AuthMessage(new Uint8Array()).encode(w, version))).rejects.toThrow();
});

test("unsupported after AUTH_OK is an AUTH_ERROR on the wire", async () => {
	const chunks: Uint8Array[] = [];
	const stream = new Stream({
		readable: new ReadableStream(),
		writable: new WritableStream<Uint8Array>({
			write: (chunk) => {
				chunks.push(new Uint8Array(chunk));
			},
		}),
		version: Lite.Version.DRAFT_07,
	});
	const wire = new LiteAuthWire({} as WebTransport, Lite.Version.DRAFT_07);
	await wire.grant(stream, { publish: patterns("room/**"), subscribe: patterns() });
	await wire.unsupported(stream, "grant cannot be encoded");
	stream.close();
	await stream.writer.closed;
	const reader = new Reader(undefined, new Uint8Array(Buffer.concat(chunks)), Lite.Version.DRAFT_07);
	expect(await decodeAuthReplyMaybe(reader, Lite.Version.DRAFT_07)).toBeInstanceOf(AuthOk);
	const reply = await decodeAuthReplyMaybe(reader, Lite.Version.DRAFT_07);
	expect(reply).toBeInstanceOf(AuthError);
	expect((reply as AuthError).code).toBe(0x30);
});

test("malformed AUTH_OK closes its session", async () => {
	for (const pattern of ["*/**", "/room", Array(33).fill("room").join("/"), undefined]) {
		const reader = await roundTrip(async (w) => {
			await Message.encode(
				w,
				async (body) => {
					await body.u53(pattern === undefined ? 0 : 1);
					if (pattern !== undefined) await body.string(pattern);
					await body.u53(0);
					await body.u62(pattern === undefined ? 1n << 53n : 0n);
				},
				{ id: AuthOk.id },
			);
		});
		await assertSessionViolation(reader);
	}
});

test("the largest shared expiry remains valid", async () => {
	const ok = new AuthOk(patterns(), patterns(), Number.MAX_SAFE_INTEGER);
	const reader = await roundTrip((w) => ok.encode(w, Lite.Version.DRAFT_07));
	expect(await decodeAuthReplyMaybe(reader, Lite.Version.DRAFT_07)).toEqual(ok);
});

/** A malformed reply closes its session with the protocol-violation code. */
async function assertSessionViolation(reader: Reader) {
	const closes: WebTransportCloseInfo[] = [];
	const quic = { close: (info: WebTransportCloseInfo) => closes.push(info) } as unknown as WebTransport;
	const wire = new LiteAuthWire(quic, Lite.Version.DRAFT_07);
	const data = await reader.readAll();
	const stream = new Stream({
		readable: new ReadableStream({
			start(controller) {
				controller.enqueue(data);
				controller.close();
			},
		}),
		writable: new WritableStream(),
		version: Lite.Version.DRAFT_07,
	});
	await expect(wire.read(stream)).rejects.toThrow();
	expect(closes).toHaveLength(1);
	expect(closes[0].closeCode).toBe(SessionCode.ProtocolViolation);
	stream.close();
}

test("malformed AUTH_ERROR reasons close their session", async () => {
	for (const reason of [new Uint8Array([0xff]), new Uint8Array(8193).fill(0x61), 1n << 53n]) {
		const reader = await roundTrip(async (w) => {
			await Message.encode(
				w,
				async (body) => {
					await body.u53(SessionCode.Unauthorized);
					await body.u62(typeof reason === "bigint" ? reason : BigInt(reason.byteLength));
					if (typeof reason !== "bigint") await body.write(reason);
				},
				{ id: AuthError.id },
			);
		});
		await assertSessionViolation(reader);
	}
});
