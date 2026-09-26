import { expect, test } from "bun:test";
import { SessionCode } from "../error.ts";
import * as Path from "../path.ts";
import { Reader, Writer } from "../stream.ts";
import { AuthError, AuthMessage, AuthOk, decodeAuthReplyMaybe, encodeAuthReply } from "./auth.ts";
import * as Lite from "./index.ts";

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
	return new Reader(undefined, buf);
}

test("AUTH round-trips its token", async () => {
	const r = await roundTrip((w) => new AuthMessage(new TextEncoder().encode("jwt")).encode(w, Lite.Version.DRAFT_06));
	const msg = await AuthMessage.decode(r, Lite.Version.DRAFT_06);
	expect(new TextDecoder().decode(msg.token)).toBe("jwt");
});

test("AUTH_OK and AUTH_ERROR round-trip behind their type", async () => {
	const ok = new AuthOk(patterns("**"), patterns(), 60_000);
	const err = new AuthError(SessionCode.Unauthorized, "expired");
	const r = await roundTrip(async (w) => {
		await encodeAuthReply(w, ok, Lite.Version.DRAFT_06);
		await encodeAuthReply(w, err, Lite.Version.DRAFT_06);
	});
	const first = await decodeAuthReplyMaybe(r, Lite.Version.DRAFT_06);
	expect(first).toBeInstanceOf(AuthOk);
	if (!(first instanceof AuthOk)) throw new Error("unreachable");
	// `**` grants everything; the empty list grants nothing.
	expect(first.publish.equals(new Path.Patterns([Path.Pattern.all()]))).toBe(true);
	expect(first.subscribe.size).toBe(0);
	expect(first.expires).toBe(60_000);

	const second = await decodeAuthReplyMaybe(r, Lite.Version.DRAFT_06);
	expect(second).toEqual(err);
	expect(await decodeAuthReplyMaybe(r, Lite.Version.DRAFT_06)).toBeUndefined();
});

test("literal and wildcard grants travel exactly, never widened", async () => {
	const ok = new AuthOk(patterns("room/alice", "room/*/cam", "**/demo.hang"), patterns("", "lobby/**"));
	const got = await AuthOk.decode(await roundTrip((w) => ok.encode(w, Lite.Version.DRAFT_06)), Lite.Version.DRAFT_06);
	expect(got.publish.equals(ok.publish)).toBe(true);
	expect(got.subscribe.equals(ok.subscribe)).toBe(true);
});

// The same bytes as `auth_ok_golden` in `rs/moq-net/src/lite/auth.rs`.
test("AUTH_OK matches the Rust encoding", async () => {
	const ok = new AuthOk(patterns("room/*/cam", "**/b.hang"), patterns(""), 1000);
	const r = await roundTrip((w) => encodeAuthReply(w, ok, Lite.Version.DRAFT_06));
	const text = (s: string) => [s.length, ...new TextEncoder().encode(s)];
	expect([...(await r.readAll())]).toEqual([
		0x00, // AUTH_OK
		0x1a, // length
		0x02, // publish count, in canonical order
		...text("**/b.hang"),
		...text("room/*/cam"),
		0x01, // subscribe count
		0x00, // the empty pattern: the root alone
		0x43,
		0xe8, // expires: 1000ms
	]);
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
		await expect(decodeAuthReplyMaybe(r, Lite.Version.DRAFT_06)).rejects.toThrow();
	}
});

test("lite-05 carries no AUTH", async () => {
	await expect(
		roundTrip((w) => new AuthMessage(new Uint8Array()).encode(w, Lite.Version.DRAFT_05)),
	).rejects.toThrow();
});
