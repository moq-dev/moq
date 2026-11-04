// Bun's --no-orphans owns every descendant, including detached fixture groups.
import { dlopen, FFIType } from "bun:ffi";
import { constants } from "node:os";

const command = Bun.argv.slice(2);
if (!command.length) throw new Error("usage: sh/local.ts COMMAND [ARGS...]");

// --no-orphans only reaches descendants still in this tree, and a grandchild whose parent
// exits first reparents out of it. Adopting orphans keeps them reachable on Linux.
if (process.platform === "linux") {
	const PR_SET_CHILD_SUBREAPER = 36;
	const libc = dlopen("libc.so.6", { prctl: { args: [FFIType.i32, FFIType.u64], returns: FFIType.i32 } });
	if (libc.symbols.prctl(PR_SET_CHILD_SUBREAPER, 1) !== 0) throw new Error("prctl(PR_SET_CHILD_SUBREAPER) failed");
}

const child = Bun.spawn(command, {
	stdin: "inherit",
	stdout: "inherit",
	stderr: "inherit",
	env: { ...process.env, LOCAL_SUPERVISED: "1" },
});
// Forward cancellation and let the child clean up; once it exits, every survivor is ours.
let signaled: number | undefined;
for (const name of ["SIGINT", "SIGTERM", "SIGHUP"] as const) {
	process.on(name, () => {
		signaled ??= 128 + constants.signals[name];
		child.kill(name);
	});
}
const code = await child.exited;
process.exit(signaled ?? code);
