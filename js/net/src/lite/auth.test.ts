import { expect, test } from "bun:test";
import { Unsupported } from "../auth.ts";
import { SessionCode } from "../error.ts";
import * as Path from "../path.ts";
import { Reader, Writer } from "../stream.ts";
import { AuthError, AuthMessage, AuthOk, decodeAuthReplyMaybe, encodeAuthReply } from "./auth.ts";
import * as Lite from "./index.ts";

function patterns(...prefixes: string[]): Path.Patterns {
	return new Path.Patterns(prefixes.map((prefix) => Path.Pattern.subtree(prefix)));
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
	const ok = new AuthOk(patterns(""), patterns(), 60_000);
	const err = new AuthError(SessionCode.Unauthorized, "expired");
	const r = await roundTrip(async (w) => {
		await encodeAuthReply(w, ok, Lite.Version.DRAFT_06);
		await encodeAuthReply(w, err, Lite.Version.DRAFT_06);
	});
	const first = await decodeAuthReplyMaybe(r, Lite.Version.DRAFT_06);
	expect(first).toBeInstanceOf(AuthOk);
	if (!(first instanceof AuthOk)) throw new Error("unreachable");
	// The empty prefix grants everything; the empty list grants nothing.
	expect(first.publish.equals(new Path.Patterns([Path.Pattern.all()]))).toBe(true);
	expect(first.subscribe.size).toBe(0);
	expect(first.expires).toBe(60_000);

	const second = await decodeAuthReplyMaybe(r, Lite.Version.DRAFT_06);
	expect(second).toEqual(err);
	expect(await decodeAuthReplyMaybe(r, Lite.Version.DRAFT_06)).toBeUndefined();
});

test("a grant the prefix wire cannot express is refused, never widened", async () => {
	const narrow = new AuthOk(new Path.Patterns([Path.Pattern.literal("room/alice")]), patterns());
	await expect(roundTrip((w) => narrow.encode(w, Lite.Version.DRAFT_06))).rejects.toBeInstanceOf(Unsupported);
});

test("lite-05 carries no AUTH", async () => {
	await expect(
		roundTrip((w) => new AuthMessage(new Uint8Array()).encode(w, Lite.Version.DRAFT_05)),
	).rejects.toThrow();
});
