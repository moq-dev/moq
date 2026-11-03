# Native headless playback

`just test native-play` runs the shipped `moq play --video off` against a local
relay and two overlapping Opus tone publishers on Linux. A private PulseAudio
null sink clocks the production ALSA/cpal output. One player process must decode
440 Hz from A, 880 Hz after B takes the same path while A stays live, then 440 Hz
again after B closes. It runs on lite-07, lite-06, and IETF draft 22, and each
player must exit cleanly on Ctrl-C. No display or physical audio device is used.

Every audio-server socket and configuration belongs to the harness run
([`../lib/pulse.py`](../lib/pulse.py)). The desktop sound server and its
settings are untouched. Nix supplies the server and client tools; the test runs
in the nightly audio job.

Each phase uses nonoverlapping 400 ms windows from the interop tone judge. Each
switch must be heard within a bound derived from its parts, counted from B's
connection (its encoder already running) or B's exit: the relay's and the
player's 300 ms announcement holds, five loopback round trips at QUIC's 1 ms
timer granularity, one 20 ms Opus frame, the 50 ms playback delay, the
monitor's 20 ms latency, and two judge windows, since one may straddle the
switch. Cold start only has a hang guard. The `pulse` rows of the
[audio quality lane](../audio-quality/README.md#native-rows) grade the same
player on the same device against budgets.
