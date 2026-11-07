/**
 * The route key session: dial the relay on lite-07 over WebTransport, so `@moq/net` proves the
 * tab's route key in SETUP when the browser can, and hold the session open.
 *
 * The relay never answers a proof, so `route-key.ts` reads the relay's log instead: `implementation`
 * names this session's SETUP there.
 *
 * @module
 */
import { Connection } from "@moq/net";

/** Dial `relay` advertising `implementation`, and resolve with why the session ended. */
export async function routeKey(relay: string, implementation: string): Promise<string> {
	const connection = await Connection.connect({
		url: new URL(relay),
		versions: ["moq-lite-07-wip"],
		implementation,
		// Only WebTransport exports keying material, so a WebSocket session would prove nothing.
		websocket: { enabled: false },
	});
	return `session closed: ${(await connection.closed) ?? "without an error"}`;
}
