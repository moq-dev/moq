import { expect, test } from "bun:test";
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
