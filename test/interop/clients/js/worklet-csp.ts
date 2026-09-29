import { join } from "node:path";
import { chromium, firefox } from "playwright";
import { workletFixture } from "../../../../js/common/worklet-fixture";

const fixture = await workletFixture(`
window.violations = [];
document.addEventListener('securitypolicyviolation', event => window.violations.push(event.blockedURI));
document.querySelector('#start').onclick = () => {
 window.done = (async () => {
  const context = new AudioContext();
  try {
   await context.resume();
   await Promise.all([context.audioWorklet.addModule(render), context.audioWorklet.addModule(capture)]);
   const output = new AudioWorkletNode(context, 'render', {outputChannelCount: [1]});
   output.port.postMessage({type: 'init-post', channels: 1, rate: context.sampleRate, latency: 100, buffered: false});
   const input = new AudioWorkletNode(context, 'capture', {numberOfOutputs: 0});
   let captured = false;
   input.port.onmessage = ({data}) => {
    captured ||= data.channels[0].some(sample => Math.abs(sample) > 0.01);
    output.port.postMessage({type: 'data', timestamp: data.timestamp, data: data.channels});
   };
   const oscillator = new OscillatorNode(context);
   oscillator.connect(input);
   oscillator.start();
   const analyser = new AnalyserNode(context);
   const mute = new GainNode(context, {gain: 0});
   output.connect(analyser).connect(mute).connect(context.destination);
   const samples = new Float32Array(analyser.fftSize);
   await new Promise(resolve => {
    const read = () => {
     analyser.getFloatTimeDomainData(samples);
     if (captured && samples.some(sample => Math.abs(sample) > 0.01)) resolve();
     else requestAnimationFrame(read);
    };
    read();
   });
   return {captured, rendered: true, violations: window.violations};
  } finally {await context.close();}
 })();
};
`);
const server = Bun.serve({
	port: 0,
	async fetch(request) {
		const path = new URL(request.url).pathname;
		const file = Bun.file(join(fixture.root, "app", path === "/" ? "index.html" : path));
		if (!(await file.exists())) return new Response(null, { status: 404 });
		return new Response(file, { headers: { "Content-Security-Policy": "script-src 'self'; object-src 'none'" } });
	},
});
try {
	for (const engine of [chromium, firefox]) {
		const browser = await engine.launch();
		try {
			const page = await browser.newPage();
			const errors: string[] = [];
			page.on("pageerror", (error) => errors.push(error.message));
			await page.goto(`http://localhost:${server.port}`);
			await page.click("#start");
			await page.evaluate(() => {
				const state = window as unknown as { done: Promise<unknown>; result?: unknown };
				void state.done.then(
					(result) => {
						state.result = result;
					},
					(error) => {
						state.result = String(error);
					},
				);
			});
			await page.waitForFunction(() => (window as unknown as { result?: unknown }).result !== undefined);
			const result = await page.evaluate(() => (window as unknown as { result: unknown }).result);
			if (
				errors.length ||
				JSON.stringify(result) !== JSON.stringify({ captured: true, rendered: true, violations: [] })
			)
				throw new Error(JSON.stringify({ result, errors }));
			console.log(
				`${engine.name()}: capture and render passed under script-src 'self' through a built Vite consumer`,
			);
		} finally {
			await browser.close();
		}
	}
} finally {
	server.stop(true);
	fixture.close();
}
