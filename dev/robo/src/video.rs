use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result};
use bytes::{Bytes, BytesMut};
use mp4_atom::{Atom, DecodeMaybe};

use crate::MediaFiles;
use crate::robo::VideoCommand;

/// A single H.264 sample extracted from an fMP4 file.
struct Sample {
    /// Presentation timestamp in microseconds.
    pts_micros: u64,
    /// Raw AVCC-format H.264 data (length-prefixed NALUs).
    data: Bytes,
    /// Whether this sample is a keyframe (IDR).
    #[allow(dead_code)]
    keyframe: bool,
}

/// An fMP4 file preloaded into memory and parsed into samples.
struct PreloadedFile {
    /// AVCDecoderConfigurationRecord for initializing decoders.
    avcc: Bytes,
    /// All video samples in decode order.
    samples: Vec<Sample>,
    /// Source video dimensions.
    width: u32,
    height: u32,
}

impl PreloadedFile {
    fn load(path: &Path) -> Result<Self> {
        let data = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        let mut cursor = std::io::Cursor::new(&data);

        let mut moov = None;
        // (moof, moof_encoded_size, mdat, mdat_header_size)
        let mut fragments: Vec<(mp4_atom::Moof, usize, mp4_atom::Mdat, usize)> = Vec::new();
        let mut pending_moof: Option<(mp4_atom::Moof, usize)> = None;

        let mut position: usize = 0;
        while let Some(atom) = mp4_atom::Any::decode_maybe(&mut cursor)? {
            let atom_end = cursor.position() as usize;
            let atom_size = atom_end - position;

            match atom {
                mp4_atom::Any::Moov(m) => moov = Some(m),
                mp4_atom::Any::Moof(m) => {
                    anyhow::ensure!(pending_moof.is_none(), "consecutive moof without mdat");
                    pending_moof = Some((m, atom_size));
                }
                mp4_atom::Any::Mdat(m) => {
                    let (moof, moof_size) = pending_moof.take().context("mdat without moof")?;
                    let mdat_header_size = atom_size - m.data.len();
                    fragments.push((moof, moof_size, m, mdat_header_size));
                }
                _ => {} // skip ftyp, styp, sidx, etc.
            }

            position = atom_end;
        }

        let moov = moov.context("missing moov atom")?;

        // Find the video track.
        let trak = moov
            .trak
            .iter()
            .find(|t| t.mdia.hdlr.handler.as_ref() == b"vide")
            .context("no video track")?;

        let track_id = trak.tkhd.track_id;
        let timescale = trak.mdia.mdhd.timescale as u64;

        // Extract AVCC and dimensions from the codec box.
        let codec = trak
            .mdia
            .minf
            .stbl
            .stsd
            .codecs
            .first()
            .context("missing codec")?;
        let avc1 = match codec {
            mp4_atom::Codec::Avc1(avc1) => avc1,
            other => anyhow::bail!("expected avc1 codec, got: {:?}", other),
        };

        let mut avcc_buf = BytesMut::new();
        avc1.avcc.encode_body(&mut avcc_buf)?;
        let avcc = avcc_buf.freeze();
        let width = avc1.visual.width as u32;
        let height = avc1.visual.height as u32;

        // Get defaults from moov.mvex.trex.
        let trex = moov
            .mvex
            .as_ref()
            .and_then(|mvex| mvex.trex.iter().find(|t| t.track_id == track_id));
        let default_sample_duration = trex.map(|t| t.default_sample_duration).unwrap_or_default();
        let default_sample_size = trex.map(|t| t.default_sample_size).unwrap_or_default();
        let default_sample_flags = trex.map(|t| t.default_sample_flags).unwrap_or_default();

        // Extract samples from all fragments.
        let mut samples = Vec::new();

        for (moof, moof_size, mdat, mdat_header_size) in &fragments {
            for traf in &moof.traf {
                if traf.tfhd.track_id != track_id {
                    continue;
                }

                let tfdt = traf.tfdt.as_ref().context("missing tfdt")?;
                let mut dts = tfdt.base_media_decode_time;

                for trun in &traf.trun {
                    let tfhd = &traf.tfhd;

                    let mut offset = if let Some(data_offset) = trun.data_offset {
                        let base = tfhd.base_data_offset.unwrap_or_default() as usize;
                        let data_offset: usize =
                            data_offset.try_into().context("invalid data offset")?;
                        // data_offset is relative to start of moof.
                        base + data_offset
                            .checked_sub(*moof_size)
                            .and_then(|v| v.checked_sub(*mdat_header_size))
                            .context("invalid data offset: underflow")?
                    } else {
                        0
                    };

                    for entry in &trun.entries {
                        let size = entry
                            .size
                            .unwrap_or(tfhd.default_sample_size.unwrap_or(default_sample_size))
                            as usize;
                        let flags = entry
                            .flags
                            .unwrap_or(tfhd.default_sample_flags.unwrap_or(default_sample_flags));
                        let duration = entry.duration.unwrap_or(
                            tfhd.default_sample_duration
                                .unwrap_or(default_sample_duration),
                        );

                        let pts = (dts as i64 + entry.cts.unwrap_or_default() as i64) as u64;
                        let pts_micros = pts * 1_000_000 / timescale;

                        anyhow::ensure!(
                            offset + size <= mdat.data.len(),
                            "sample extends beyond mdat"
                        );
                        let sample_data = Bytes::copy_from_slice(&mdat.data[offset..offset + size]);

                        // Keyframe detection (same logic as fmp4.rs).
                        let keyframe = {
                            let kf = (flags >> 24) & 0x3 == 0x2;
                            let non_sync = (flags >> 16) & 0x1 == 0x1;
                            kf && !non_sync
                        };

                        samples.push(Sample {
                            pts_micros,
                            data: sample_data,
                            keyframe,
                        });

                        dts += duration as u64;
                        offset += size;
                    }
                }
            }
        }

        anyhow::ensure!(
            !samples.is_empty(),
            "no video samples found in {}",
            path.display()
        );

        tracing::info!(
            path = %path.display(),
            samples = samples.len(),
            width,
            height,
            "preloaded file"
        );

        Ok(Self {
            avcc,
            samples,
            width,
            height,
        })
    }
}

/// Result of processing samples from a file.
enum FileResult {
    Eof { last_pts: i64 },
    Interrupted { cmd: VideoCommand, last_pts: i64 },
}

/// What the pipeline is currently playing.
enum PlayState {
    Idle,
    Dead,
    Action(String),
}

/// Preload all media files into RAM, then run the async playback loop.
pub async fn run_pipeline(
    media: MediaFiles,
    broadcast: moq_lite::BroadcastProducer,
    catalog: moq_mux::CatalogProducer,
    mut cmd_rx: tokio::sync::watch::Receiver<Option<VideoCommand>>,
    done_tx: tokio::sync::mpsc::Sender<()>,
) -> Result<()> {
    ffmpeg_next::init().context("failed to init ffmpeg")?;

    // Preload all files into RAM (blocking, once at startup).
    let files: BTreeMap<String, PreloadedFile> = tokio::task::block_in_place(|| {
        let mut files = BTreeMap::new();
        files.insert("idle".into(), PreloadedFile::load(&media.idle)?);
        files.insert("dead".into(), PreloadedFile::load(&media.dead)?);
        for (name, path) in &media.actions {
            files.insert(name.clone(), PreloadedFile::load(path)?);
        }
        Ok::<_, anyhow::Error>(files)
    })?;

    let mut hd = moq_mux::import::Avc1::new(broadcast.clone(), catalog.clone());

    // Spawn the 240p transcoder on a dedicated thread.
    let preview = moq_mux::import::Avc3::new(broadcast, catalog);
    let mut transcoder = TranscoderHandle::spawn(preview);

    let mut pts_offset: i64 = 0;
    let mut state = PlayState::Idle;

    loop {
        let (file_key, looping) = match &state {
            PlayState::Idle => ("idle", true),
            PlayState::Dead => ("dead", true),
            PlayState::Action(name) => (name.as_str(), false),
        };

        let file = files
            .get(file_key)
            .with_context(|| format!("unknown file: {file_key}"))?;

        tracing::info!(?state, looping, samples = file.samples.len(), "playing");

        match process_samples(
            file,
            &mut hd,
            &mut transcoder,
            &mut cmd_rx,
            looping,
            pts_offset,
        )
        .await?
        {
            FileResult::Eof { last_pts } => {
                pts_offset = last_pts + 33_333;

                if looping {
                    tokio::task::yield_now().await;
                    continue;
                }

                // Action finished — notify robo.
                done_tx.send(()).await.context("done channel closed")?;
                cmd_rx.changed().await.context("command channel closed")?;

                match cmd_rx.borrow_and_update().as_ref() {
                    Some(VideoCommand::Action(name)) => state = PlayState::Action(name.clone()),
                    Some(VideoCommand::Kill) => state = PlayState::Dead,
                    None => state = PlayState::Idle,
                }
            }
            FileResult::Interrupted { cmd, last_pts } => {
                pts_offset = last_pts + 33_333;
                match cmd {
                    VideoCommand::Action(name) => state = PlayState::Action(name),
                    VideoCommand::Kill => state = PlayState::Dead,
                }
            }
        }
    }
}

async fn process_samples(
    file: &PreloadedFile,
    hd: &mut moq_mux::import::Avc1,
    transcoder: &mut TranscoderHandle,
    cmd_rx: &mut tokio::sync::watch::Receiver<Option<VideoCommand>>,
    looping: bool,
    pts_offset: i64,
) -> Result<FileResult> {
    // Initialize HD track from AVCC.
    hd.initialize(&mut file.avcc.as_ref())?;

    // Initialize the 240p transcoder for this file.
    transcoder
        .init(file.avcc.clone(), file.width, file.height)
        .await?;

    let wall_start = tokio::time::Instant::now();
    let pts_start = pts_offset;
    let mut last_pts = pts_offset;

    for sample in &file.samples {
        // Check for commands (non-blocking).
        if cmd_rx.has_changed().unwrap_or(false) {
            let cmd = cmd_rx.borrow_and_update().clone();
            if let Some(cmd) = cmd {
                match &cmd {
                    VideoCommand::Kill => {
                        return Ok(FileResult::Interrupted { cmd, last_pts });
                    }
                    VideoCommand::Action(_) if looping => {
                        return Ok(FileResult::Interrupted { cmd, last_pts });
                    }
                    VideoCommand::Action(_) => {} // robo handles queueing
                }
            }
        }

        let adjusted_pts = sample.pts_micros as i64 + pts_offset;

        // Pace to real-time (async — yields to runtime, ctrl+c works).
        let target = std::time::Duration::from_micros((adjusted_pts - pts_start).max(0) as u64);
        tokio::time::sleep_until(wall_start + target).await;

        let ts = hang::container::Timestamp::from_micros(adjusted_pts.max(0) as u64)
            .context("timestamp overflow")?;

        // HD: feed AVCC packet to Avc1 (sync, non-blocking memory write).
        hd.decode(&mut sample.data.as_ref(), Some(ts))?;

        // 240p: send to transcoder thread (non-blocking, bounded channel provides backpressure).
        transcoder.frame(sample.data.clone(), ts).await?;

        last_pts = adjusted_pts;
    }

    // Wait for the transcoder to flush.
    transcoder.flush(last_pts).await?;

    Ok(FileResult::Eof { last_pts })
}

// --- Transcoder thread ---

/// Message sent to the transcoder thread.
enum TranscoderMsg {
    /// (Re)initialize the decoder/encoder for a new file.
    Init {
        avcc: Bytes,
        width: u32,
        height: u32,
        reply: tokio::sync::oneshot::Sender<Result<()>>,
    },
    /// Transcode a single frame.
    Frame {
        data: Bytes,
        ts: hang::container::Timestamp,
    },
    /// Flush the decoder/encoder at end of file.
    Flush {
        last_pts: i64,
        reply: tokio::sync::oneshot::Sender<Result<()>>,
    },
}

/// Async handle to the transcoder thread.
struct TranscoderHandle {
    tx: tokio::sync::mpsc::Sender<TranscoderMsg>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl TranscoderHandle {
    /// Spawn the transcoder on a dedicated thread.
    fn spawn(preview: moq_mux::import::Avc3) -> Self {
        let (tx, rx) = tokio::sync::mpsc::channel(4);

        let thread = std::thread::Builder::new()
            .name("transcoder".into())
            .spawn(move || transcoder_thread(rx, preview))
            .expect("failed to spawn transcoder thread");

        Self {
            tx,
            thread: Some(thread),
        }
    }

    async fn init(&self, avcc: Bytes, width: u32, height: u32) -> Result<()> {
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        self.tx
            .send(TranscoderMsg::Init {
                avcc,
                width,
                height,
                reply: reply_tx,
            })
            .await
            .context("transcoder thread dead")?;
        reply_rx.await.context("transcoder thread dead")?
    }

    async fn frame(&self, data: Bytes, ts: hang::container::Timestamp) -> Result<()> {
        self.tx
            .send(TranscoderMsg::Frame { data, ts })
            .await
            .context("transcoder thread dead")
    }

    async fn flush(&self, last_pts: i64) -> Result<()> {
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        self.tx
            .send(TranscoderMsg::Flush {
                last_pts,
                reply: reply_tx,
            })
            .await
            .context("transcoder thread dead")?;
        reply_rx.await.context("transcoder thread dead")?
    }
}

impl Drop for TranscoderHandle {
    fn drop(&mut self) {
        // Drop the sender to signal the thread to exit.
        // (tx is dropped automatically, thread sees channel closed)
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Runs on a dedicated thread. Owns the ffmpeg decoder/encoder/scaler (non-Send types)
/// and the Avc3 preview track.
fn transcoder_thread(
    mut rx: tokio::sync::mpsc::Receiver<TranscoderMsg>,
    mut preview: moq_mux::import::Avc3,
) {
    let mut state: Option<Transcoder> = None;

    while let Some(msg) = rx.blocking_recv() {
        match msg {
            TranscoderMsg::Init {
                avcc,
                width,
                height,
                reply,
            } => {
                let result = Transcoder::new(&avcc, width, height);
                match result {
                    Ok(t) => {
                        state = Some(t);
                        let _ = reply.send(Ok(()));
                    }
                    Err(e) => {
                        let _ = reply.send(Err(e));
                    }
                }
            }
            TranscoderMsg::Frame { data, ts } => {
                if let Some(t) = state.as_mut() {
                    if let Err(e) = t.transcode_frame(&data, ts, &mut preview) {
                        tracing::error!(error = %e, "transcode error");
                    }
                }
            }
            TranscoderMsg::Flush { last_pts, reply } => {
                let result = if let Some(t) = state.as_mut() {
                    t.flush(&mut preview, last_pts)
                } else {
                    Ok(())
                };
                state = None; // Drop the transcoder after flush.
                let _ = reply.send(result);
            }
        }
    }
}

/// Encapsulates ffmpeg decode → scale → encode for the 240p preview.
/// Lives on the dedicated transcoder thread (never crosses thread boundaries).
struct Transcoder {
    decoder: ffmpeg_next::decoder::Video,
    encoder: ffmpeg_next::encoder::video::Encoder,
    scaler: ffmpeg_next::software::scaling::Context,
    frame_count: u64,
}

impl Transcoder {
    fn new(avcc: &[u8], src_width: u32, src_height: u32) -> Result<Self> {
        // Create decoder with H.264 codec and AVCC extradata.
        let codec = ffmpeg_next::decoder::find(ffmpeg_next::codec::Id::H264)
            .context("H.264 decoder not found")?;
        let mut ctx = ffmpeg_next::codec::Context::new_with_codec(codec);

        // Set extradata (AVCC) via raw FFI.
        unsafe {
            let ptr = ctx.as_mut_ptr();
            let extradata = ffmpeg_next::ffi::av_malloc(avcc.len()) as *mut u8;
            anyhow::ensure!(!extradata.is_null(), "av_malloc failed");
            std::ptr::copy_nonoverlapping(avcc.as_ptr(), extradata, avcc.len());
            (*ptr).extradata = extradata;
            (*ptr).extradata_size = avcc.len() as i32;
            (*ptr).width = src_width as i32;
            (*ptr).height = src_height as i32;
        }

        let decoder = ctx.decoder().video()?;

        // 240p encoder — match source aspect ratio.
        let sd_height: u32 = 240;
        let sd_width = (sd_height as u64 * src_width as u64 / src_height as u64) as u32 & !1;

        let enc_codec = ffmpeg_next::encoder::find(ffmpeg_next::codec::Id::H264)
            .context("H.264 encoder not found")?;
        let enc_ctx = ffmpeg_next::codec::Context::new_with_codec(enc_codec);
        let mut enc = enc_ctx.encoder().video()?;
        enc.set_width(sd_width);
        enc.set_height(sd_height);
        enc.set_format(ffmpeg_next::format::Pixel::YUV420P);
        enc.set_time_base(ffmpeg_next::Rational::new(1, 30));
        enc.set_frame_rate(Some(ffmpeg_next::Rational::new(30, 1)));
        enc.set_bit_rate(300_000);
        enc.set_gop(60);

        let mut opts = ffmpeg_next::Dictionary::new();
        opts.set("preset", "ultrafast");
        opts.set("tune", "zerolatency");
        let encoder = enc.open_with(opts)?;

        let scaler = ffmpeg_next::software::scaling::Context::get(
            ffmpeg_next::format::Pixel::YUV420P,
            src_width,
            src_height,
            ffmpeg_next::format::Pixel::YUV420P,
            sd_width,
            sd_height,
            ffmpeg_next::software::scaling::Flags::BILINEAR,
        )?;

        Ok(Self {
            decoder,
            encoder,
            scaler,
            frame_count: 0,
        })
    }

    fn transcode_frame(
        &mut self,
        data: &Bytes,
        ts: hang::container::Timestamp,
        preview: &mut moq_mux::import::Avc3,
    ) -> Result<()> {
        let mut packet = ffmpeg_next::Packet::copy(data);
        packet.set_pts(Some(self.frame_count as i64));
        packet.set_dts(Some(self.frame_count as i64));

        self.decoder.send_packet(&packet)?;

        let mut decoded = ffmpeg_next::frame::Video::empty();
        while self.decoder.receive_frame(&mut decoded).is_ok() {
            let mut yuv = ffmpeg_next::frame::Video::empty();
            self.scaler.run(&decoded, &mut yuv)?;
            burn_label(&mut yuv);
            yuv.set_pts(Some(self.frame_count as i64));

            if self.frame_count == 0 {
                yuv.set_kind(ffmpeg_next::picture::Type::I);
            }

            self.encoder.send_frame(&yuv)?;
            self.drain_encoder(preview, ts)?;
            self.frame_count += 1;
        }

        Ok(())
    }

    fn flush(&mut self, preview: &mut moq_mux::import::Avc3, last_pts: i64) -> Result<()> {
        let ts =
            hang::container::Timestamp::from_micros(last_pts.max(0) as u64).context("timestamp")?;

        self.decoder.send_eof()?;
        let mut decoded = ffmpeg_next::frame::Video::empty();
        while self.decoder.receive_frame(&mut decoded).is_ok() {
            let mut yuv = ffmpeg_next::frame::Video::empty();
            self.scaler.run(&decoded, &mut yuv)?;
            yuv.set_pts(Some(self.frame_count as i64));
            self.encoder.send_frame(&yuv)?;
            self.drain_encoder(preview, ts)?;
            self.frame_count += 1;
        }

        self.encoder.send_eof()?;
        self.drain_encoder(preview, ts)?;
        Ok(())
    }

    fn drain_encoder(
        &mut self,
        preview: &mut moq_mux::import::Avc3,
        ts: hang::container::Timestamp,
    ) -> Result<()> {
        let mut pkt = ffmpeg_next::Packet::empty();
        while self.encoder.receive_packet(&mut pkt).is_ok() {
            let data = pkt.data().context("empty encoded packet")?;
            preview.decode_frame(&mut &*data, Some(ts))?;
        }
        Ok(())
    }
}

/// Burn a "240p" label in the bottom-right corner of a YUV420P frame.
fn burn_label(frame: &mut ffmpeg_next::frame::Video) {
    let w = frame.width() as usize;
    let h = frame.height() as usize;
    let scale = 2usize;

    #[rustfmt::skip]
    let glyphs: &[&[u8; 7]] = &[
        &[0b01110, 0b10001, 0b00001, 0b00110, 0b01000, 0b10000, 0b11111], // 2
        &[0b00010, 0b00110, 0b01010, 0b10010, 0b11111, 0b00010, 0b00010], // 4
        &[0b01110, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110], // 0
        &[0b11110, 0b10001, 0b10001, 0b11110, 0b10000, 0b10000, 0b10000], // p
    ];

    let glyph_w = 5;
    let glyph_h = 7;
    let spacing = 1;
    let padding = 3;

    let text_w = (glyphs.len() * (glyph_w + spacing) - spacing) * scale;
    let text_h = glyph_h * scale;
    let box_w = text_w + padding * 2;
    let box_h = text_h + padding * 2;

    let x0 = w.saturating_sub(box_w + 4);
    let y0 = h.saturating_sub(box_h + 4);

    let y_stride = frame.stride(0);
    let y_data = frame.data_mut(0);

    for y in y0..y0 + box_h {
        for x in x0..x0 + box_w {
            if x < w && y < h {
                y_data[y * y_stride + x] = 30;
            }
        }
    }

    let text_x = x0 + padding;
    let text_y = y0 + padding;

    for (ci, glyph) in glyphs.iter().enumerate() {
        for (row, &bits) in glyph.iter().enumerate() {
            for col in 0..glyph_w {
                if bits & (1 << (glyph_w - 1 - col)) != 0 {
                    for dy in 0..scale {
                        for dx in 0..scale {
                            let px = text_x + ci * (glyph_w + spacing) * scale + col * scale + dx;
                            let py = text_y + row * scale + dy;
                            if px < w && py < h {
                                y_data[py * y_stride + px] = 220;
                            }
                        }
                    }
                }
            }
        }
    }
}

impl std::fmt::Debug for PlayState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PlayState::Idle => write!(f, "idle"),
            PlayState::Dead => write!(f, "dead"),
            PlayState::Action(name) => write!(f, "action:{name}"),
        }
    }
}
