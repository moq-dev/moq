// A cold fallback request first has to fetch and evaluate its module.
Object.assign(globalThis, {
	async measureQmux(url: string) {
		const start = performance.now();
		const { default: Session } = await import("@moq/qmux");
		const session = new Session(url);
		await session.ready;
		const elapsed = performance.now() - start;
		session.close();
		return elapsed;
	},
});
