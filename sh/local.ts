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
// Every process below `root`, from one process table snapshot.
function descendants(root: number): number[] {
	const children = new Map<number, number[]>();
	for (const line of Bun.spawnSync(["ps", "-A", "-o", "pid=,ppid="]).stdout.toString().split("\n")) {
		const [pid, ppid] = line.trim().split(/\s+/).map(Number);
		if (pid && ppid !== undefined) children.set(ppid, [...(children.get(ppid) ?? []), pid]);
	}
	const found: number[] = [];
	for (let queue = [root]; queue.length; ) {
		for (const pid of children.get(queue.pop() as number) ?? []) {
			found.push(pid);
			queue.push(pid);
		}
	}
	return found;
}

// Forward cancellation and let the child clean up; once it exits, every survivor is ours.
// Without a subreaper (macOS), a descendant whose parent dies leaves the tree before
// --no-orphans sweeps it, so record the tree when cancellation arrives.
let signaled: number | undefined;
const tree = new Set<number>();
for (const name of ["SIGINT", "SIGTERM", "SIGHUP"] as const) {
	process.on(name, () => {
		signaled ??= 128 + constants.signals[name];
		if (process.platform !== "linux") for (const pid of descendants(process.pid)) tree.add(pid);
		child.kill(name);
	});
}
const code = await child.exited;
for (const pid of tree) {
	try {
		process.kill(pid, "SIGKILL");
	} catch {
		// Already gone.
	}
}
process.exit(signaled ?? code);
