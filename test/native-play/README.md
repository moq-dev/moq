# Native headless playback

`just test native-play` runs the shipped `moq play --no-video` against a local
relay and two overlapping Opus tone publishers on Linux. A private PulseAudio
null sink clocks the production ALSA/cpal output. One player process must decode
440 Hz from A, 880 Hz after B takes the same path while A stays live, then 440 Hz
again after B closes. It runs on lite-07, lite-06, and IETF draft 22, and each
player must exit cleanly on Ctrl-C. No display or physical audio device is used.

Every audio-server socket and configuration belongs to the harness run
([`../lib/pulse.py`](../lib/pulse.py)). The desktop sound server and its
settings are untouched. Nix supplies the server and client tools; the test runs
in the nightly audio job.

Each phase uses nonoverlapping 400 ms windows from the interop tone judge. A
window straddling the switch may fail the tone check before the next complete
window proves the new source. Logged switch times include that observation
cadence, encoder/container startup for B, and the
player's explicitly configured 50 ms delay. The 30 second watchdog detects a
hang; it is not an RTT performance budget. The `pulse` rows of the
[audio quality lane](../audio-quality/README.md#native-rows) grade the same
player on the same device against budgets.
