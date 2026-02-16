---
title: AI
description: Welcome to the future, old man.
---

# AI
Hopefully you had this square on your buzzword bingo card.

WebRTC is a great protocol for conferencing, but it's not designed for AI.
But I haven't personally worked in this space either so take my suggestions with a grain of salt.

## Latency
Inference is still quite slow and expensive, even for the big players.
If you're going to spend >300ms and literal dollars on expensive inference, you want at least *some* reliability guarantees.

Unfortunately, WebRTC will never try to retransmit audio packets.
A single lost packet will cause noticeable audio distortion.
And if you have the audacity to generate audio/video separately, WebRTC won't synchronize them for you.
Frames are rendered on receipt, so unless you introduce a delay, audio will be out of sync with video.

One of the core tenets of MoQ is adjustable latency.
The viewer (and thus your application) controls how long it's willing to wait for content before it gets skipped/desynced.
The latency budget of the network protocol can match the latency budget of the application.

## On-Demand
MoQ is pull-based, so nothing is transmitted over the network until there's at least one subscriber.
You can further extend this by not generating/encoding content either.

Both of these were mentioned briefly on the [contribution](/concept/use-case/contribution) page if you want to read more.

### Inference
If you want to save compute resources, you can defer inference until it's actually needed.

For example, let's say you're publishing a `captions` track populated by Whisper or something.
If nobody has enabled captions, then nobody will subscribe to the `captions` track.
You can stop generating the track (or use a smaller model) until it's actually requested.

### Simulcast
If you want to save bandwidth, you can publish media in a format expected by the AI model.

For example, let's say you're doing object detection on a bunch of security cameras.
The model inputs video at 360p and 10fps or something like that, so that's what you publish.
But if a human (those still exist) wants to audit the full video, you can separately serve the full resolution video.
Since this is on-demand, you will only encode/transmit the 1080p video when it's actually needed.

## Browser Control
One of the perks of using WebSockets/MoQ instead of WebRTC is that you get full control over the media pipeline.

[WebCodecs](https://developer.mozilla.org/en-US/docs/Web/API/WebCodecs_API) is used to encode/decode media within the browser.
- For video, you use [VideoFrame](https://developer.mozilla.org/en-US/docs/Web/API/VideoFrame) which directly maps to a texture on the GPU. You can use WebGPU to perform inference, encoding, rendering, etc without ever touching the CPU.
- For audio, you get [AudioData](https://developer.mozilla.org/en-US/docs/Web/API/AudioData) which is (usually) just a float32 array. You control exactly how these are processed, captured, emitted, etc.

It's more work to do this instead of using a `<video>` element of course, but it opens the door to more possibilities.
Run additional inference in the browser, render your media to textures on a model, etc.

And note that all of this is possible with WebRTC and [insertable streams](https://developer.mozilla.org/en-US/docs/Web/API/Insertable_Streams_for_MediaStreamTrack_API).
However, you're really not gaining much by using WebRTC only for networking... just use MoQ instead.

## Non-Media
MoQ is not just for media.

Send your prompts over the same WebTransport connection as the media.
Or send non-media stuff like vertex data for 3D models, separate from the texture data.
It's a versatile protocol with a wide range of use-cases.

## Simplicity
You're working with AI, so you're probably building something new.

If you don't want to deal with SDP, or connections that take 10 RTTs, or unsupported media encodings, or STUN/TURN servers, then give MoQ a try.
It's a lot closer to WebSockets than WebRTC, but with the ability to skip and scale.
