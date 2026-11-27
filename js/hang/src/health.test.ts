import { expect, test } from "bun:test";
import { Duration } from "@moq/clock";
import * as Echo from "./echo";
import * as Health from "./health";
import * as Stats from "./stats";

interface Case {
	name: string;
	role: Health.Role;
	previous?: { observer: Health.Observer; snapshot: unknown };
	current: { observer: Health.Observer; snapshot: unknown };
	interval: number;
	age: number;
	health: unknown;
}

const cases = (await Bun.file(new URL("../../../rs/hang/fixtures/health.json", import.meta.url)).json()) as Case[];

function grade(c: Case, thresholds?: Health.Thresholds): Health.Health {
	if (c.role === "publisher") {
		const report = (r: Case["current"]) => ({
			observer: r.observer,
			snapshot: Stats.SnapshotSchema.parse(r.snapshot),
		});
		const sample = {
			previous: c.previous && report(c.previous),
			current: report(c.current),
			interval: Duration.millis(c.interval),
			age: Duration.millis(c.age),
		};
		return Health.publisher(sample, thresholds);
	}
	const report = (r: Case["current"]) => ({ observer: r.observer, snapshot: Echo.SnapshotSchema.parse(r.snapshot) });
	const sample = {
		previous: c.previous && report(c.previous),
		current: report(c.current),
		interval: Duration.millis(c.interval),
		age: Duration.millis(c.age),
	};
	return Health.viewer(sample, thresholds);
}

for (const c of cases) {
	test(`shared Rust fixture grades as recorded: ${c.name}`, () => {
		expect(JSON.parse(JSON.stringify(grade(c)))).toEqual(c.health);
	});
}

test("caller thresholds override the defaults", () => {
	const c = cases.find((c) => c.name.startsWith("viewer healthy"));
	if (!c) throw new Error("missing fixture");
	const thresholds = Health.defaultThresholds();
	thresholds.transport.rtt.degraded = Duration.millis(50);
	const health = grade(c, thresholds);
	expect(health.transport.verdict).toBe("degraded");
	expect(health.verdict).toBe("degraded");
	expect(Health.defaultThresholds().transport.rtt.degraded.asMillis()).toBe(300);
});

test("a rendition named __proto__ is graded like any other", () => {
	const snapshot = (frames: number, skipped: number) =>
		JSON.parse(`{"renditions":{"__proto__":{"frames":${frames},"skipped":${skipped}}}}`) as Stats.Snapshot;
	const observer = cases[0].current.observer;
	const health = Health.publisher({
		previous: { observer, snapshot: snapshot(100, 0) },
		current: { observer, snapshot: snapshot(200, 10) },
		interval: Duration.secs(1),
		age: Duration.ZERO,
	});
	expect(Object.keys(health.renditions)).toEqual(["__proto__"]);
	expect(health.verdict).toBe("unhealthy");
});

test("a bitrate past u64 saturates where Rust's does", () => {
	const snapshot = (bytes: number) => Stats.SnapshotSchema.parse({ renditions: { video: { bytes } } });
	const observer = cases[0].current.observer;
	const health = Health.publisher({
		previous: { observer, snapshot: snapshot(0) },
		current: { observer, snapshot: snapshot(Number.MAX_SAFE_INTEGER) },
		interval: Duration.millis(1),
		age: Duration.ZERO,
	});
	expect(health.renditions.video.bitrate).toBe(Number("18446744073709551615"));
});

test("a fractional interval reports whole milliseconds, as Rust's does", () => {
	const c = cases.find((c) => (c.health as Health.Health).interval !== undefined);
	if (!c) throw new Error("missing fixture");
	const health = grade({ ...c, interval: c.interval + 0.5 });
	expect(health.interval).toBe(c.interval);
});
