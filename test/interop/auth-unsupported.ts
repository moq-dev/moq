// The Rust interop preflight sends a grant followed by NOT_SUPPORTED, then reads ours back.
import { Unsupported } from "../../js/net/src/auth.ts";
import { LiteAuthWire } from "../../js/net/src/lite/auth.ts";
import { Version } from "../../js/net/src/lite/version.ts";
import { Stream } from "../../js/net/src/stream.ts";

const bytes = Uint8Array.from(JSON.parse(process.argv[2]) as number[]);
const encoded: Uint8Array[] = [];
const stream = new Stream({
	readable: new ReadableStream<Uint8Array>({
		start(controller) {
			controller.enqueue(bytes);
			controller.close();
		},
	}),
	writable: new WritableStream<Uint8Array>({
		write(chunk) {
			encoded.push(new Uint8Array(chunk));
		},
	}),
	version: Version.DRAFT_07,
});
const wire = new LiteAuthWire({} as WebTransport, Version.DRAFT_07);
const first = await wire.read(stream);
if (!first || !("grant" in first) || !first.grant.publish.matches("room/cam")) throw new Error("Rust grant missing");
const second = await wire.read(stream);
if (!second || !("refused" in second) || !(second.refused instanceof Unsupported))
	throw new Error("Rust unsupported lost");
await wire.grant(stream, first.grant);
await wire.unsupported(stream, "unsupported");
await stream.writer.closed;
process.stdout.write(JSON.stringify([...Buffer.concat(encoded)]));
