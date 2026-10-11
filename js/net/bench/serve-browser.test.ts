import { expect, mock, spyOn, test } from "bun:test";
import { chromium } from "playwright";

test("stops the benchmark server when Chromium fails to launch", async () => {
	const stop = mock();
	const serve = spyOn(Bun, "serve").mockReturnValue({ stop } as unknown as ReturnType<typeof Bun.serve>);
	const launch = spyOn(chromium, "launch").mockRejectedValue(new Error("launch failed"));
	try {
		await expect(import("./serve-browser")).rejects.toThrow("launch failed");
		expect(stop).toHaveBeenCalledWith(true);
	} finally {
		serve.mockRestore();
		launch.mockRestore();
	}
});
