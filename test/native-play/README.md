# Native headless playback

`just test native-play` runs the shipped `moq play --no-video` against a local
relay and an Opus tone publisher on Linux. A private PulseAudio null sink clocks
the production ALSA/cpal output; its monitor must receive nonzero PCM, and the
player must exit cleanly on Ctrl-C. No display or physical audio device is used.

Every audio-server socket and configuration belongs to the harness run. The
desktop sound server and its settings are untouched. Nix supplies the server
and client tools; the test runs in the nightly audio job.

This is a playback smoke test, not a quality score. It does not measure
per-callback underruns, stalls, decoder stages, or a native timing floor, and
does not populate the audio-quality metric schema or budgets.
