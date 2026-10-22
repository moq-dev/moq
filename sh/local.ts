// Bun's --no-orphans owns every descendant, including detached fixture groups.
import { constants } from "node:os";

const command = Bun.argv.slice(2);
if (!command.length) throw new Error("usage: sh/local.ts COMMAND [ARGS...]");
const child = Bun.spawn(command, {
	stdin: "inherit",
	stdout: "inherit",
	stderr: "inherit",
	env: { ...process.env, MOQ_LOCAL_SUPERVISED: "1" },
});
for (const name of ["SIGINT", "SIGTERM", "SIGHUP"] as const) {
	process.on(name, () => {
		child.kill(name);
		process.exit(128 + constants.signals[name]);
	});
}
process.exit(await child.exited);
