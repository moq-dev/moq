import type { ConsoleMessage, Page } from "playwright";

/** Page identity within one benchmark run. */
export interface Identity {
	variant: string;
	kind: string;
	role: string;
	key: string;
	round: number;
	pass: string;
}

/** Retains page diagnostics independently of whether a sample completes. */
export class Consoles {
	#sink: Bun.FileSink;
	#next = 0;
	#pending = new Set<Promise<void>>();
	#error: unknown;

	constructor(path: string) {
		this.#sink = Bun.file(path).writer();
	}

	/** Listen before navigation so startup failures leave their diagnostics too. */
	attach(page: Page, identity: Identity) {
		const id = this.#next++;
		const record = (type: string, text: string, location?: ReturnType<ConsoleMessage["location"]>) => {
			if (this.#error) return;
			try {
				const written = this.#sink.write(
					`${JSON.stringify({ ...identity, page: id, time: Date.now(), type, text, location })}\n`,
				);
				if (written instanceof Promise) {
					const pending = written
						.catch((error: unknown) => {
							this.#error ??= error;
						})
						.then(() => {
							this.#pending.delete(pending);
						});
					this.#pending.add(pending);
				}
			} catch (error) {
				this.#error ??= error;
			}
		};
		const console = (message: ConsoleMessage) => record(message.type(), message.text(), message.location());
		const error = (error: Error) => record("pageerror", error.stack ?? error.message);
		page.on("console", console);
		page.on("pageerror", error);
		page.once("close", () => {
			page.off("console", console);
			page.off("pageerror", error);
		});
	}

	/** Finish received writes and surface any failure to retain the logs. */
	async [Symbol.asyncDispose]() {
		await Promise.all(this.#pending);
		try {
			await this.#sink.end();
		} catch (error) {
			this.#error ??= error;
		}
		if (this.#error) throw this.#error;
	}
}
