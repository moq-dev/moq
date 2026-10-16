/** Drive the real player and software WebCodecs decoder through an open-GOP join and later delivery hole. @module */
import { expect, test } from "bun:test";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { build } from "vite";
import { worklet } from "../../../../js/common/vite-plugin-worklet";
import { launch } from "./harness";
import type { Packet } from "./open-gop/main";

test("software video recovery trims orphan pictures after join and later gap", async () => {
	const run = mkdtempSync(join(tmpdir(), "moq-open-gop-"));
	const file = join(run, "fixture.ts");
	const command = (cmd: string[]) => {
		const result = Bun.spawnSync(cmd, { stderr: "pipe", stdout: "pipe" });
		if (result.exitCode !== 0) throw new Error(new TextDecoder().decode(result.stderr));
		return new TextDecoder().decode(result.stdout);
	};
	const browser = await launch();
	let server: ReturnType<typeof Bun.serve> | undefined;
	try {
		command([
			"ffmpeg",
			"-hide_banner",
			"-loglevel",
			"error",
			"-f",
			"lavfi",
			"-i",
			"testsrc2=size=160x96:rate=30",
			"-frames:v",
			"72",
			"-c:v",
			"libx264",
			"-threads",
			"2",
			"-profile:v",
			"high",
			"-level:v",
			"3.1",
			"-pix_fmt",
			"yuv420p",
			"-x264-params",
			"keyint=12:min-keyint=12:scenecut=0:open-gop=1:bframes=3:b-adapt=0:repeat-headers=1",
			"-f",
			"mpegts",
			file,
		]);
		const probe = JSON.parse(
			command([
				"ffprobe",
				"-v",
				"error",
				"-select_streams",
				"v",
				"-show_packets",
				"-show_data",
				"-of",
				"json",
				file,
			]),
		) as { packets: Array<{ pts_time: string; flags: string; data: string }> };
		let group = -1;
		const all: Packet[] = probe.packets.map((packet) => {
			if (packet.flags.includes("K")) group++;
			const hex = packet.data
				.split("\n")
				.filter((line) => line.includes(":"))
				.map((line) =>
					line
						.slice(line.indexOf(":") + 1)
						.trimStart()
						.split("  ")[0]
						.replaceAll(" ", ""),
				)
				.join("");
			return {
				group,
				timestamp: Math.round(Number(packet.pts_time) * 1_000_000),
				data: Buffer.from(hex, "hex").toString("base64"),
			};
		});
		const packets = all.filter((packet) => [1, 2, 4, 5].includes(packet.group));
		const floors = new Map<number, number>();
		for (const packet of packets) if (!floors.has(packet.group)) floors.set(packet.group, packet.timestamp);
		const orphan = (packet: Packet) =>
			[1, 4].includes(packet.group) && packet.timestamp < (floors.get(packet.group) ?? 0);
		expect(packets.filter(orphan).length).toBeGreaterThan(0);
		const expected = packets.filter((packet) => !orphan(packet)).map((packet) => packet.timestamp);
		const dist = join(run, "dist");
		await build({
			configFile: false,
			root: join(import.meta.dir, "open-gop"),
			logLevel: "warn",
			plugins: [worklet()],
			build: { target: "esnext", outDir: dist, emptyOutDir: true },
		});
		server = Bun.serve({
			port: 0,
			hostname: "127.0.0.1",
			async fetch(req) {
				const path = new URL(req.url).pathname;
				const file = Bun.file(join(dist, path === "/" ? "index.html" : path));
				return (await file.exists()) ? new Response(file) : new Response("not found", { status: 404 });
			},
		});
		const page = await browser.newPage();
		page.on("pageerror", (error) => console.error(error));
		page.on("console", (message) => {
			if (message.type() === "error") console.error(message.text());
		});
		await page.goto(`http://127.0.0.1:${server.port}`);
		await page.waitForFunction(() => typeof window.runRecovery === "function");
		const result = await page.evaluate((packets) => window.runRecovery(packets), packets);
		expect(result.software).toBe(true);
		expect(result.submitted).toEqual(expected);
		expect(result.decoded).toBeGreaterThan(0);
	} finally {
		await browser.close();
		server?.stop();
		rmSync(run, { recursive: true, force: true });
	}
}, 60_000);
