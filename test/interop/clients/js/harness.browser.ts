import { expect, test } from "bun:test";
import { createServer } from "vite";
import { launch, pause } from "./harness";

test("pause activates its button when a canvas intercepts pointer input", async () => {
	const browser = await launch();
	try {
		const page = await browser.newPage();
		await page.setContent(`
			<moq-watch-ui></moq-watch-ui>
			<script>
				const root = document.querySelector("moq-watch-ui").attachShadow({ mode: "open" });
				root.innerHTML = \`<button class="control" aria-label="Pause">Pause</button>
					<canvas style="position: fixed; inset: 0; width: 100%; height: 100%"></canvas>\`;
				root.querySelector("button").addEventListener("click", () => {
					document.body.dataset.paused = "true";
				});
			</script>
		`);
		await pause(page);
		expect(await page.locator("body").getAttribute("data-paused")).toBe("true");
	} finally {
		await browser.close();
	}
});

for (const stall of ["audio", "video"]) {
	test(`the paused play button stays clickable during ${stall} stalling`, async () => {
		const server = await createServer({
			configFile: false,
			root: `${import.meta.dir}/buffering`,
			server: { host: "127.0.0.1", port: 0 },
		});
		await server.listen();
		const browser = await launch();
		try {
			const page = await browser.newPage();
			await page.goto(`${server.resolvedUrls?.local[0]}?${stall === "video" ? "video" : ""}`);
			const spinner = page.locator(".buffering");
			await spinner.waitFor({ state: "visible" });
			await page.getByRole("button", { name: "Pause", exact: true }).click();
			await page.locator('body[data-paused="true"]').waitFor();
			await spinner.waitFor({ state: "hidden" });

			// Click normally: a forced click would miss the overlay intercepting pointer input.
			await page.locator(".center-play").click({ timeout: 5000 });
			await page.locator('body[data-paused="false"]').waitFor();
			await spinner.waitFor({ state: "visible" });
		} finally {
			await browser.close();
			await server.close();
		}
	}, 30_000);
}
