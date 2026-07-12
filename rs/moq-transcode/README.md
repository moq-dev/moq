# moq-transcode

Just-in-time live transcoding for hang broadcasts.

Consume a source broadcast, publish a derivative broadcast next to it: the
derivative catalog advertises lower renditions (rungs) of the source video plus
relative references back to the source renditions, so a player picks from the
combined ladder and the transcoder never proxies what it doesn't re-encode.

Nothing is encoded until someone asks, Cloudflare-style just-in-time per rung:

- **Subscribe** to a rung and the transcoder subscribes to the source track,
  decoding, scaling, and re-encoding group for group until the last subscriber
  leaves.
- **Fetch** a specific group and the transcoder fetches that same group from
  the source and transcodes just that group. Output groups mirror source group
  sequence numbers 1:1, so group N of every rung is the same content as source
  group N and rendition switches land cleanly.

The codec work is [`moq-video`](../moq-video): hardware where available (NVDEC +
NVENC on Linux, VideoToolbox on macOS, Media Foundation on Windows), openh264 as
the H.264 software fallback. H.264, H.265, and 8-bit 4:2:0 AV1 source renditions
are eligible when a matching decoder is available. On an NVIDIA GPU the pipeline
is fully GPU-resident: NVDEC decodes and scales in hardware and NVENC encodes the
CUDA frame in place, with no CPU copies. Other decoders fall back to CPU scaling.

## Library

One entry point: `run(source, output, config)`. The caller owns session setup
and where the derivative is announced; the transcoder only fills the output
broadcast.

```rust
let mut config = moq_transcode::Config::default();
// The derivative is announced at `<source>/transcode.hang`, so the source
// renditions are referenced one level up.
config.source = Some(moq_net::PathRelativeOwned::from("..".to_string()));

let output = moq_net::broadcast::Info::default().produce();
let announce = origin.publish_broadcast(format!("{path}/transcode.hang"), &output)?;

moq_transcode::run(source, output, config).await?;
drop(announce);
```

Only renditions strictly below the source survive the ladder: a 480p source is
never transcoded up to 720p, and a same-height rung is only offered when it
undercuts a known source bitrate.

## Example

Publish something first (e.g. `moq publish camera` from
[`moq-cli`](../moq-cli)), then:

```bash
cargo run -p moq-transcode --example transcode -- \
    --url http://localhost:4443/anon --source my-broadcast
```

The derivative appears at `my-broadcast/transcode.hang`.
