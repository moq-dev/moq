/** Datagrams from a QMux session over the browser's native `WebSocketStream`. @module */
import { Session } from "../../../../../js/qmux/src/index.ts";

/** Dial `url`, send `count` one-byte datagrams back to back, then a stream that marks the end. */
export async function run(url: string, protocol: string, count: number, burst: boolean): Promise<void> {
	if (typeof (globalThis as { WebSocketStream?: unknown }).WebSocketStream !== "function")
		throw new Error("no native WebSocketStream");
	const session = new Session(url, {
		protocols: [protocol],
		versions: { [protocol]: "qmux-01" },
		requireProtocol: true,
	});
	await session.ready;
	const datagrams = session.datagrams.writable.getWriter();
	if (burst) await Promise.all(Array.from({ length: count }, (_, i) => datagrams.write(new Uint8Array([i]))));
	else for (let i = 0; i < count; i++) await datagrams.write(new Uint8Array([i]));
	const end = (await session.createUnidirectionalStream()).getWriter();
	await end.write(new Uint8Array([0]));
	await end.close();
}
