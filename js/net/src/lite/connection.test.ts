import { expect, test } from "bun:test";
import { probeLevel } from "./connection.ts";
import { ProbeLevel } from "./setup.ts";
import { Version } from "./version.ts";

/** A transport whose `getStats` behaves as described, or is absent entirely. */
function transport(getStats?: () => Promise<unknown>): WebTransport {
	return (getStats ? { getStats } : {}) as unknown as WebTransport;
}

// `Report` claims we can measure and periodically report. The qmux/WebSocket
// fallback implements no `getStats()`, so a publisher there has nothing to send;
// advertising Report and then holding the subscriber's PROBE stream open with
// nothing on it is the state this avoids.
test("no getStats advertises None", async () => {
	expect(await probeLevel(transport(), Version.DRAFT_05)).toBe(ProbeLevel.None);
});

// Having the method is not the same as having a measurement.
test("getStats with no usable metric advertises None", async () => {
	const quic = transport(async () => ({ estimatedSendRate: null }));
	expect(await probeLevel(quic, Version.DRAFT_05)).toBe(ProbeLevel.None);
});

test("either metric alone is enough to advertise Report", async () => {
	const rateOnly = transport(async () => ({ estimatedSendRate: 1_000_000 }));
	expect(await probeLevel(rateOnly, Version.DRAFT_05)).toBe(ProbeLevel.Report);

	const rttOnly = transport(async () => ({ estimatedSendRate: null, smoothedRtt: 12.34 }));
	expect(await probeLevel(rttOnly, Version.DRAFT_05)).toBe(ProbeLevel.Report);
});

// lite-03's PROBE has no RTT field, so an RTT is not something we could report
// there even though we can measure it.
test("an RTT alone is not reportable on a version that cannot carry one", async () => {
	const rttOnly = transport(async () => ({ estimatedSendRate: null, smoothedRtt: 12.34 }));
	expect(await probeLevel(rttOnly, Version.DRAFT_03)).toBe(ProbeLevel.None);
});

// A transport that cannot answer tells us nothing, which is itself an answer. A
// throwing getStats must not escape into the SETUP path.
test("a throwing getStats advertises None rather than propagating", async () => {
	const quic = transport(async () => {
		throw new Error("no stats for you");
	});
	expect(await probeLevel(quic, Version.DRAFT_05)).toBe(ProbeLevel.None);
});
