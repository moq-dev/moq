import Session from "@moq/qmux";

// The entrypoint has already loaded the fallback before a connection is requested.
Object.assign(globalThis, {
	async measureQmux(url: string) {
		const start = performance.now();
		const session = new Session(url);
		await session.ready;
		const elapsed = performance.now() - start;
		session.close();
		return elapsed;
	},
});
