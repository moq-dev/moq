import { expect, test } from "bun:test";
import { Reader, Writer } from "../stream.ts";
import * as Message from "./message.ts";
import { RequestError } from "./request.ts";
import { type IetfVersion, Version } from "./version.ts";

/** A writer that keeps every chunk its stream is handed. */
function recording(version: IetfVersion): { writer: Writer; chunks: Uint8Array[] } {
	const chunks: Uint8Array[] = [];
	const writer = new Writer(
		new WritableStream<Uint8Array>({ write: (chunk) => void chunks.push(new Uint8Array(chunk)) }),
		version,
	);
	return { writer, chunks };
}

test("a control message goes out in one write, and one that fails to encode writes nothing", async () => {
	for (const version of [Version.DRAFT_18]) {
		const { writer, chunks } = recording(version);
		const err = new RequestError({ errorCode: 2, reasonPhrase: "nope" });
		await Message.write(writer, RequestError.id, err, version);
		expect(chunks.length).toBe(1);

		const reader = new Reader(undefined, chunks[0], version);
		expect(await reader.u53()).toBe(RequestError.id);
		const decoded = await RequestError.decode(reader, version);
		expect(decoded.reasonPhrase).toBe("nope");
		expect(await reader.done()).toBe(true);

		// A message written without a type, as on a stream whose header named it, is one write too.
		await err.encode(writer, version);
		expect(chunks.length).toBe(2);

		const huge = new RequestError({ errorCode: 2, reasonPhrase: "x".repeat(65_536) });
		await expect(Message.write(writer, RequestError.id, huge, version)).rejects.toThrow("Message too large");
		expect(chunks.length).toBe(2);
	}
});
