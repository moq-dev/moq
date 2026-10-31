/** Runs wasm-bindgen's browser tests with the existing Playwright Chromium. */
import { chromium } from "playwright";

// Interactive mode exposes the same wasm-bindgen harness without requiring a
// separate WebDriver binary or a second browser installation.
const runner = Bun.spawn(["wasm-bindgen-test-runner", ...process.argv.slice(2)], {
	env: { ...process.env, NO_HEADLESS: "1", WASM_BINDGEN_TEST_ADDRESS: "127.0.0.1:0" },
	stdout: "pipe",
	stderr: "inherit",
});
let browser: Awaited<ReturnType<typeof chromium.launch>> | undefined;
try {
	const reader = runner.stdout.getReader();
	const decoder = new TextDecoder();
	let output = "";
	let url: string | undefined;
	while (!url) {
		const { done, value } = await reader.read();
		if (done) throw new Error(`wasm-bindgen runner exited before serving tests (${await runner.exited})`);
		const chunk = decoder.decode(value, { stream: true });
		process.stdout.write(chunk);
		output += chunk;
		url = /Interactive browsers tests are now available at (http:\/\/[^\s]+)/.exec(output)?.[1];
	}
	reader.releaseLock();
	browser = await chromium.launch({ headless: true });
	const page = await browser.newPage();
	page.on("pageerror", (error) => console.error(error));
	await page.goto(url);
	await page.waitForFunction(() => document.getElementById("output")?.textContent?.includes("test result: "));
	// biome-ignore lint/performance/useDomNodeTextContent: Playwright's Locator.innerText(), not the DOM property.
	const result = await page.locator("#output").innerText();
	console.log(result);
	if (!/test result: ok\. [1-9]\d* passed; 0 failed;/.test(result)) {
		throw new Error("Rust browser tests failed or no tests ran");
	}
} finally {
	await browser?.close();
	runner.kill();
	await runner.exited;
}
