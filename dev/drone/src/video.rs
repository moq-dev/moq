use anyhow::{Context, Result};
use bytes::Bytes;

use std::sync::atomic::Ordering;

use crate::game::{self, GameState};
use crate::sensor::BatteryLevel;

const WIDTH: u32 = 720;
const HEIGHT: u32 = 720;
const SD_WIDTH: u32 = 360;
const SD_HEIGHT: u32 = 360;

/// Run the game loop: render frames, encode H.264, publish via MoQ.
pub async fn run_pipeline(
    broadcast: moq_lite::BroadcastProducer,
    catalog: moq_mux::CatalogProducer,
    mut cmd_rx: tokio::sync::mpsc::Receiver<String>,
    battery: BatteryLevel,
) -> Result<()> {
    ffmpeg_next::init().context("failed to init ffmpeg")?;

    // Two Avc3 tracks: HD and SD.
    let hd = moq_mux::import::Avc3::new(broadcast.clone(), catalog.clone());
    let sd = moq_mux::import::Avc3::new(broadcast, catalog);

    let encoder = EncoderHandle::spawn(hd, sd);

    let mut game = GameState::new();
    let mut pixmap = tiny_skia::Pixmap::new(WIDTH, HEIGHT).context("failed to create pixmap")?;

    let mut interval = tokio::time::interval(std::time::Duration::from_micros(33_333)); // ~30fps
    let start = tokio::time::Instant::now();

    loop {
        interval.tick().await;

        // Drain all pending commands (non-blocking).
        while let Ok(name) = cmd_rx.try_recv() {
            if let Some(action) = game::GameAction::from_str(&name) {
                game.apply_action(action);
            }
        }

        // Advance physics.
        game.tick();

        // Sync battery level to sensor track.
        battery.store(game.battery() as u32, Ordering::Relaxed);

        // Render frame.
        game.render(&mut pixmap);

        // Encode and publish.
        let rgba = Bytes::copy_from_slice(pixmap.data());
        let pts_micros = start.elapsed().as_micros() as u64;
        let ts =
            hang::container::Timestamp::from_micros(pts_micros).context("timestamp overflow")?;
        encoder.frame(rgba, ts).await?;
    }
}

// --- Encoder thread ---

enum EncoderMsg {
    Frame {
        rgba: Bytes,
        ts: hang::container::Timestamp,
    },
}

struct EncoderHandle {
    tx: tokio::sync::mpsc::Sender<EncoderMsg>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl EncoderHandle {
    fn spawn(hd: moq_mux::import::Avc3, sd: moq_mux::import::Avc3) -> Self {
        let (tx, rx) = tokio::sync::mpsc::channel(4);

        let thread = std::thread::Builder::new()
            .name("encoder".into())
            .spawn(move || encoder_thread(rx, hd, sd))
            .expect("failed to spawn encoder thread");

        Self {
            tx,
            thread: Some(thread),
        }
    }

    async fn frame(&self, rgba: Bytes, ts: hang::container::Timestamp) -> Result<()> {
        self.tx
            .send(EncoderMsg::Frame { rgba, ts })
            .await
            .context("encoder thread dead")
    }
}

impl Drop for EncoderHandle {
    fn drop(&mut self) {
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn encoder_thread(
    mut rx: tokio::sync::mpsc::Receiver<EncoderMsg>,
    mut hd: moq_mux::import::Avc3,
    mut sd: moq_mux::import::Avc3,
) {
    let mut hd_enc: Option<Encoder> = None;
    let mut sd_enc: Option<Encoder> = None;
    let mut rgba_scaler: Option<ffmpeg_next::software::scaling::Context> = None;
    let mut sd_scaler: Option<ffmpeg_next::software::scaling::Context> = None;

    while let Some(msg) = rx.blocking_recv() {
        let EncoderMsg::Frame { rgba, ts } = msg;

        // Lazy-init.
        let hd_encoder = lazy_init(
            &mut hd_enc,
            || Encoder::new(WIDTH, HEIGHT, 500_000),
            "HD encoder",
        );
        let sd_encoder = lazy_init(
            &mut sd_enc,
            || Encoder::new(SD_WIDTH, SD_HEIGHT, 200_000),
            "SD encoder",
        );
        let color_scaler = lazy_init(
            &mut rgba_scaler,
            || {
                ffmpeg_next::software::scaling::Context::get(
                    ffmpeg_next::format::Pixel::RGBA,
                    WIDTH,
                    HEIGHT,
                    ffmpeg_next::format::Pixel::YUV420P,
                    WIDTH,
                    HEIGHT,
                    ffmpeg_next::software::scaling::Flags::BILINEAR,
                )
                .map_err(Into::into)
            },
            "RGBA scaler",
        );
        let downscaler = lazy_init(
            &mut sd_scaler,
            || {
                ffmpeg_next::software::scaling::Context::get(
                    ffmpeg_next::format::Pixel::YUV420P,
                    WIDTH,
                    HEIGHT,
                    ffmpeg_next::format::Pixel::YUV420P,
                    SD_WIDTH,
                    SD_HEIGHT,
                    ffmpeg_next::software::scaling::Flags::BILINEAR,
                )
                .map_err(Into::into)
            },
            "SD scaler",
        );

        let (Some(hd_encoder), Some(sd_encoder), Some(color_scaler), Some(downscaler)) =
            (hd_encoder, sd_encoder, color_scaler, downscaler)
        else {
            return; // Init failed, error already logged.
        };

        // RGBA → YUV420P at 720p.
        let yuv_hd = match rgba_to_yuv(&rgba, color_scaler, hd_encoder.frame_count) {
            Ok(f) => f,
            Err(e) => {
                tracing::error!(error = %e, "RGBA→YUV failed");
                continue;
            }
        };

        // Burn "720p" label and encode HD.
        let mut hd_frame = yuv_hd.clone();
        burn_label(&mut hd_frame, "720p");
        if let Err(e) = hd_encoder.encode_yuv(&hd_frame, ts, &mut hd) {
            tracing::error!(error = %e, "HD encode error");
        }

        // Downscale to 360p, burn label, encode SD.
        let mut sd_frame = ffmpeg_next::frame::Video::empty();
        if let Err(e) = downscaler.run(&yuv_hd, &mut sd_frame) {
            tracing::error!(error = %e, "downscale error");
            continue;
        }
        burn_label(&mut sd_frame, "360p");
        sd_frame.set_pts(Some(sd_encoder.frame_count as i64));
        if sd_encoder.frame_count == 0 {
            sd_frame.set_kind(ffmpeg_next::picture::Type::I);
        }
        if let Err(e) = sd_encoder.encode_yuv(&sd_frame, ts, &mut sd) {
            tracing::error!(error = %e, "SD encode error");
        }
    }
}

/// Lazy-initialize an Option, logging on failure.
fn lazy_init<'a, T>(
    slot: &'a mut Option<T>,
    init: impl FnOnce() -> Result<T>,
    name: &str,
) -> Option<&'a mut T> {
    if slot.is_none() {
        match init() {
            Ok(v) => *slot = Some(v),
            Err(e) => {
                tracing::error!(error = %e, "{name} init failed");
                return None;
            }
        }
    }
    slot.as_mut()
}

/// Convert RGBA pixels to a YUV420P frame at source resolution (720p).
fn rgba_to_yuv(
    rgba: &[u8],
    scaler: &mut ffmpeg_next::software::scaling::Context,
    frame_count: u64,
) -> Result<ffmpeg_next::frame::Video> {
    let mut rgba_frame =
        ffmpeg_next::frame::Video::new(ffmpeg_next::format::Pixel::RGBA, WIDTH, HEIGHT);
    let stride = rgba_frame.stride(0);
    let row_bytes = WIDTH as usize * 4;

    for y in 0..HEIGHT as usize {
        let src_offset = y * row_bytes;
        let dst_offset = y * stride;
        rgba_frame.data_mut(0)[dst_offset..dst_offset + row_bytes]
            .copy_from_slice(&rgba[src_offset..src_offset + row_bytes]);
    }

    let mut yuv = ffmpeg_next::frame::Video::empty();
    scaler.run(&rgba_frame, &mut yuv)?;
    yuv.set_pts(Some(frame_count as i64));

    if frame_count == 0 {
        yuv.set_kind(ffmpeg_next::picture::Type::I);
    }

    Ok(yuv)
}

/// H.264 encoder for one rendition.
struct Encoder {
    encoder: ffmpeg_next::encoder::video::Encoder,
    frame_count: u64,
}

impl Encoder {
    fn new(width: u32, height: u32, bitrate: usize) -> Result<Self> {
        let codec = ffmpeg_next::encoder::find(ffmpeg_next::codec::Id::H264)
            .context("H.264 encoder not found")?;
        let ctx = ffmpeg_next::codec::Context::new_with_codec(codec);
        let mut enc = ctx.encoder().video()?;
        enc.set_width(width);
        enc.set_height(height);
        enc.set_format(ffmpeg_next::format::Pixel::YUV420P);
        enc.set_time_base(ffmpeg_next::Rational::new(1, 30));
        enc.set_frame_rate(Some(ffmpeg_next::Rational::new(30, 1)));
        enc.set_bit_rate(bitrate);
        enc.set_gop(60);

        let mut opts = ffmpeg_next::Dictionary::new();
        opts.set("preset", "ultrafast");
        opts.set("tune", "zerolatency");
        let encoder = enc.open_with(opts)?;

        Ok(Self {
            encoder,
            frame_count: 0,
        })
    }

    /// Encode a YUV frame and publish via Avc3.
    fn encode_yuv(
        &mut self,
        yuv: &ffmpeg_next::frame::Video,
        ts: hang::container::Timestamp,
        output: &mut moq_mux::import::Avc3,
    ) -> Result<()> {
        self.encoder.send_frame(yuv)?;

        let mut pkt = ffmpeg_next::Packet::empty();
        while self.encoder.receive_packet(&mut pkt).is_ok() {
            let data = pkt.data().context("empty encoded packet")?;
            output.decode_frame(&mut &*data, Some(ts))?;
        }
        self.frame_count += 1;

        Ok(())
    }
}

/// Burn a resolution label (e.g. "720p") in the bottom-right corner of a YUV420P frame.
fn burn_label(frame: &mut ffmpeg_next::frame::Video, text: &str) {
    let w = frame.width() as usize;
    let h = frame.height() as usize;
    let scale = if w > 400 { 2 } else { 1 };

    // Simple 5x7 bitmap font for digits and 'p'.
    #[rustfmt::skip]
    let glyph = |c: char| -> &'static [u8; 7] {
        match c {
            '0' => &[0b01110, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110],
            '1' => &[0b00100, 0b01100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110],
            '2' => &[0b01110, 0b10001, 0b00001, 0b00110, 0b01000, 0b10000, 0b11111],
            '3' => &[0b01110, 0b10001, 0b00001, 0b00110, 0b00001, 0b10001, 0b01110],
            '4' => &[0b00010, 0b00110, 0b01010, 0b10010, 0b11111, 0b00010, 0b00010],
            '5' => &[0b11111, 0b10000, 0b11110, 0b00001, 0b00001, 0b10001, 0b01110],
            '6' => &[0b01110, 0b10000, 0b10000, 0b11110, 0b10001, 0b10001, 0b01110],
            '7' => &[0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b01000, 0b01000],
            '8' => &[0b01110, 0b10001, 0b10001, 0b01110, 0b10001, 0b10001, 0b01110],
            '9' => &[0b01110, 0b10001, 0b10001, 0b01111, 0b00001, 0b00001, 0b01110],
            'p' => &[0b11110, 0b10001, 0b10001, 0b11110, 0b10000, 0b10000, 0b10000],
            _   => &[0b00000, 0b00000, 0b00000, 0b00000, 0b00000, 0b00000, 0b00000],
        }
    };

    let glyph_w = 5;
    let glyph_h = 7;
    let spacing = 1;
    let padding = 3;
    let chars: Vec<char> = text.chars().collect();

    let text_w = (chars.len() * (glyph_w + spacing) - spacing) * scale;
    let text_h = glyph_h * scale;
    let box_w = text_w + padding * 2;
    let box_h = text_h + padding * 2;

    let x0 = w.saturating_sub(box_w + 4);
    let y0 = h.saturating_sub(box_h + 4);

    let y_stride = frame.stride(0);
    let y_data = frame.data_mut(0);

    // Draw dark background box.
    for y in y0..y0 + box_h {
        for x in x0..x0 + box_w {
            if x < w && y < h {
                y_data[y * y_stride + x] = 30;
            }
        }
    }

    // Draw glyphs.
    let text_x = x0 + padding;
    let text_y = y0 + padding;

    for (ci, &ch) in chars.iter().enumerate() {
        let g = glyph(ch);
        for (row, &bits) in g.iter().enumerate() {
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
