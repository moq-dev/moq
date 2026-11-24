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
// Each process's parent and start time; the start time tells a live process from a
// later one that reused its PID.
function table(): Map<number, { ppid: number; start: string }> {
	const found = new Map<number, { ppid: number; start: string }>();
	for (const line of Bun.spawnSync(["ps", "-A", "-o", "pid=,ppid=,lstart="]).stdout.toString().split("\n")) {
		const [pid, ppid, ...start] = line.trim().split(/\s+/);
		if (pid && ppid && start.length) found.set(Number(pid), { ppid: Number(ppid), start: start.join(" ") });
	}
	return found;
}

// Every process below `root` with its start time, from one snapshot.
function descendants(root: number): Map<number, string> {
	const processes = table();
	const found = new Map<number, string>();
	for (let queue = [root]; queue.length; ) {
		const parent = queue.pop() as number;
		for (const [pid, { ppid, start }] of processes) {
			if (ppid !== parent || found.has(pid)) continue;
			found.set(pid, start);
			queue.push(pid);
		}
	}
	return found;
}

// Forward cancellation and let the child clean up; once it exits, every survivor is ours.
// Without a subreaper (macOS), a descendant whose parent dies leaves the tree before
// --no-orphans sweeps it, so record the tree when cancellation arrives.
let signaled: number | undefined;
const tree = new Map<number, string>();
for (const name of ["SIGINT", "SIGTERM", "SIGHUP"] as const) {
	process.on(name, () => {
		signaled ??= 128 + constants.signals[name];
		if (process.platform !== "linux") for (const [pid, start] of descendants(process.pid)) tree.set(pid, start);
		child.kill(name);
	});
}
const code = await child.exited;
if (tree.size) {
	// Kill only a recorded process that keeps its start time, never a PID's new owner.
	const now = table();
	for (const [pid, start] of tree) {
		if (now.get(pid)?.start !== start) continue;
		try {
			process.kill(pid, "SIGKILL");
		} catch {
			// Exited since the check.
		}
	}
}
process.exit(signaled ?? code);
