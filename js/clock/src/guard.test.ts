import { expect, test } from "bun:test";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const src = import.meta.dir;

// Each case runs as its own test under the real preload, since the guard fails a test from a host callback.
const fixture = `
import { test } from "bun:test";
import { Milli, Nano, sleep, timeout, yieldNow } from "${join(src, "index.ts")}";
import { hostTime } from "${join(src, "test.ts")}";

const ms = (n) => Nano.fromMilli(Milli(n));

test("waits 50ms on the real clock", async () => {
	await sleep(ms(50));
});
test("races sleep to prove nothing happened", async () => {
	await Promise.race([new Promise(() => {}), sleep(ms(5))]);
});
test("a watchdog on progress that settles first", async () => {
	await timeout(ms(1000), Promise.resolve(1));
});
test("a sleep already due", async () => {
	await sleep(Nano(0));
});
test("opted out with hostTime", async () => {
	hostTime();
	await sleep(ms(5));
});
test("leaks a timer past its end", () => {
	void sleep(ms(5));
});
test("runs while an earlier test's timer fires", async () => {
	const end = performance.now() + 20;
	while (performance.now() < end) await yieldNow();
});
`;

test("the preload fails exactly the tests whose own timer fires on the host clock", () => {
	const dir = mkdtempSync(join(tmpdir(), "moq-clock-guard-"));
	try {
		writeFileSync(join(dir, "bunfig.toml"), `[test]\npreload = [${JSON.stringify(join(src, "preload.ts"))}]\n`);
		writeFileSync(join(dir, "fixture.test.ts"), fixture);
		const run = Bun.spawnSync(["bun", "test", "--reporter=junit", "--reporter-outfile=report.xml"], { cwd: dir });
		const report = readFileSync(join(dir, "report.xml"), "utf8");
		const results = Object.fromEntries(
			[...report.matchAll(/<testcase name="([^"]+)"[^>]*?(\/?)>/g)].map(([, name, closed]) => [
				name.replaceAll("&apos;", "'"),
				closed ? "pass" : "fail",
			]),
		);
		expect(results).toEqual({
			"waits 50ms on the real clock": "fail",
			"races sleep to prove nothing happened": "fail",
			"a watchdog on progress that settles first": "pass",
			"a sleep already due": "pass",
			"opted out with hostTime": "pass",
			"leaks a timer past its end": "pass",
			"runs while an earlier test's timer fires": "pass",
		});
		expect(run.stderr.toString()).toContain("a timer fired on the host clock");
	} finally {
		rmSync(dir, { recursive: true, force: true });
	}
});
