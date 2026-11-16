// Bun ships no WebCodecs, so a test answers the sources' support probes with a stand-in decoder.

type Answer<Config> = (config: Config) => boolean | Promise<boolean>;

/** The probes to answer: each one given replaces that decoder until the stub is disposed. */
export interface Answers {
	video?: Answer<VideoDecoderConfig>;
	audio?: Answer<AudioDecoderConfig>;
}

/**
 * Install `VideoDecoder` and `AudioDecoder` globals whose `isConfigSupported` answers as given.
 * The audio stub comes with an `AudioEncoder` too, so the libav polyfill sees WebCodecs and stays unloaded.
 */
export function stubSupport(answers: Answers): Disposable {
	const saved = new Map<string, PropertyDescriptor | undefined>();
	const install = (name: string, value: unknown) => {
		saved.set(name, Object.getOwnPropertyDescriptor(globalThis, name));
		Object.defineProperty(globalThis, name, { value, configurable: true, writable: true });
	};

	const { video, audio } = answers;
	if (video) {
		install(
			"VideoDecoder",
			class {
				static async isConfigSupported(config: VideoDecoderConfig) {
					return { supported: await video(config), config };
				}
			},
		);
	}
	if (audio) {
		install(
			"AudioDecoder",
			class {
				static async isConfigSupported(config: AudioDecoderConfig) {
					return { supported: await audio(config), config };
				}
			},
		);
		if (!globalThis.AudioEncoder) install("AudioEncoder", class {});
	}

	return {
		[Symbol.dispose]() {
			for (const [name, descriptor] of saved) {
				if (descriptor) Object.defineProperty(globalThis, name, descriptor);
				else delete (globalThis as Record<string, unknown>)[name];
			}
		},
	};
}
