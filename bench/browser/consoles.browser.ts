import { expect, test } from "bun:test";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { digest } from "./fixture.ts";

// Exercise the real driver and browser with local pages, without needing a relay.
for (const failure of ["none", "startup", "publish", "pageerror"]) {
	test(`page consoles survive ${failure === "none" ? "successful samples" : `${failure} failure`}`, async () => {
		const root = await mkdtemp(join(tmpdir(), "moq-browser-consoles-"));
		let running: { kill(): void; exited: Promise<number>; exitCode: number | null } | undefined;
		try {
			const dist = join(root, "dist");
			await mkdir(join(dist, "current"), { recursive: true });
			await writeFile(
				join(dist, "current", "transport.js"),
				`
console.log("page loaded");
console.warn("page warning");
window.environment = () => {
  if (${JSON.stringify(failure)} === "startup") throw new Error("fixture startup failure");
  return { visibility: document.visibilityState };
};
window.transport = {
  publish() {
    if ("__copies" in window) throw new Error("publisher must not install copy counters");
    console.error("publisher diagnostic");
    if (${JSON.stringify(failure)} === "publish") throw new Error("fixture publish failure");
  },
  prepare({full}) {
    if (("__copies" in window) !== full) throw new Error("consumer copy counters do not match the pass");
    console.log("consumer ready");
    return "fixture";
  },
  consume({size, frames}) {
    return { frames, bytes: size * frames,
      digest: size === 160 ? ${digest(160, 200)} : ${digest(131072, 16)},
      elapsed_ms: 1, long_tasks: {count: 0, total_ms: 0} };
  },
  stop() { console.log("page stopped"); }
};
if (${JSON.stringify(failure)} === "pageerror") throw new Error("fixture pageerror failure");
`,
			);
			const child = Bun.spawn(
				[
					process.execPath,
					join(import.meta.dir, "driver.ts"),
					"--relay",
					"http://127.0.0.1:1",
					"--dist",
					dist,
					"--out",
					root,
					"--variants",
					"current",
					"--scenarios",
					"transport",
					"--smoke",
				],
				{ stdout: "pipe", stderr: "pipe" },
			);
			running = child;
			const [code, , stderr] = await Promise.all([
				child.exited,
				new Response(child.stdout).text(),
				new Response(child.stderr).text(),
			]);
			if (failure === "none") expect(code, stderr).toBe(0);
			else {
				expect(code).not.toBe(0);
				expect(stderr).toContain(`fixture ${failure} failure`);
			}
			const entries = (await readFile(join(root, "consoles.jsonl"), "utf8"))
				.trim()
				.split("\n")
				.map((line) => JSON.parse(line));
			expect(entries.some((entry) => entry.type === "warning" && entry.text === "page warning")).toBe(true);
			const loaded = entries.filter((entry) => entry.text === "page loaded");
			expect(new Set(loaded.map((entry) => entry.page)).size).toBe(loaded.length);
			expect(loaded[0]).toMatchObject({
				kind: "transport",
				key: "transport 160B x200",
				role: "publisher",
				variant: "current",
				round: 0,
				pass: "timed",
			});
			expect(Number.isFinite(loaded[0].time)).toBe(true);
			expect(entries.some((entry) => "counted" in entry)).toBe(false);
			if (failure !== "startup")
				expect(loaded[1]).toMatchObject({
					kind: "transport",
					key: "transport 160B x200",
					role: "consumer",
					variant: "current",
					round: 0,
					pass: "timed",
				});
			if (failure === "pageerror")
				expect(
					entries.some(
						(entry) => entry.type === "pageerror" && entry.text.includes("fixture pageerror failure"),
					),
				).toBe(true);
			const report = JSON.parse(await readFile(join(root, "report.json"), "utf8"));
			expect(report.samples.length).toBe(failure === "none" ? 6 : 0);
			if (failure === "none") {
				expect(entries.some((entry) => entry.type === "error" && entry.text === "publisher diagnostic")).toBe(
					true,
				);
				expect(loaded.length).toBe(12);
				// Traced and counted samples run after every round, as round -1, with both roles on each pass.
				const detailed = loaded.filter((entry) => entry.pass !== "timed");
				expect(detailed.every((entry) => entry.round === -1)).toBe(true);
				for (const pass of ["traced", "counted"])
					for (const role of ["publisher", "consumer"])
						expect(detailed.some((entry) => entry.pass === pass && entry.role === role)).toBe(true);
				expect(entries.filter((entry) => entry.text === "page stopped").length).toBe(12);
			}
		} finally {
			if (running?.exitCode === null) {
				running.kill();
				await running.exited;
			}
			await rm(root, { recursive: true, force: true });
		}
	}, 60_000);
}
