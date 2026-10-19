// The Rust interop preflight sends lite-07 AUTH and SUBSCRIBE bytes naming the largest
// nameable Auth ID, plus an AUTH carrying the one that cannot be named, and reads ours back.
import { AuthMessage } from "../../js/net/src/lite/auth.ts";
import { Subscribe } from "../../js/net/src/lite/subscribe.ts";
import { Version } from "../../js/net/src/lite/version.ts";
import { Reader, Writer } from "../../js/net/src/stream.ts";

const input = JSON.parse(process.argv[2]) as { auth: number[]; subscribe: number[]; refused: number[] };
const version = Version.DRAFT_07;

const reader = (bytes: number[]) => new Reader(undefined, Uint8Array.from(bytes), version);
async function encode(write: (w: Writer) => Promise<void>): Promise<number[]> {
	const chunks: Uint8Array[] = [];
	const writer = new Writer(
		new WritableStream<Uint8Array>({
			write(chunk) {
				chunks.push(new Uint8Array(chunk));
			},
		}),
		version,
	);
	await write(writer);
	writer.close();
	await writer.closed;
	return [...Buffer.concat(chunks)];
}

const auth = await AuthMessage.decode(reader(input.auth), version);
const subscribe = await Subscribe.decode(reader(input.subscribe), version);
const refused = await AuthMessage.decode(reader(input.refused), version).then(
	() => false,
	() => true,
);

process.stdout.write(
	JSON.stringify({
		id: auth.id.toString(),
		named: subscribe.auth?.toString(),
		auth: await encode((w) => auth.encode(w, version)),
		subscribe: await encode((w) => subscribe.encode(w, version)),
		refused,
	}),
);
