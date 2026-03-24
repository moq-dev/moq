use std::path::PathBuf;

use anyhow::{Context, Result};

/// Two-rendition pipeline:
/// - HD: transmux via Avc1 (AVCC packets passed through from demuxer)
/// - 240p: transcode via Avc3 (decode → scale → encode → Annex B)
pub fn run_pipeline(
    angles: Vec<PathBuf>,
    broadcast: moq_lite::BroadcastProducer,
    catalog: moq_mux::CatalogProducer,
    mut angle_rx: tokio::sync::watch::Receiver<usize>,
) -> Result<()> {
    ffmpeg_next::init().context("failed to init ffmpeg")?;

    let mut hd = moq_mux::import::Avc1::new(broadcast.clone(), catalog.clone());
    let mut preview = moq_mux::import::Avc3::new(broadcast, catalog);

    let mut current_angle = 1usize;
    let mut pts_offset: i64 = 0;

    loop {
        let angle_index = current_angle.saturating_sub(1).min(angles.len() - 1);
        let path = &angles[angle_index];
        tracing::info!(?path, angle = current_angle, "opening video file");

        match process_file(
            path,
            &mut hd,
            &mut preview,
            &mut angle_rx,
            &mut current_angle,
            pts_offset,
        ) {
            Ok(last_pts) => {
                pts_offset = last_pts + 33_333;
            }
            Err(e) => {
                tracing::error!(error = %e, "video pipeline error");
                return Err(e);
            }
        }
    }
}

fn process_file(
    path: &PathBuf,
    hd: &mut moq_mux::import::Avc1,
    preview: &mut moq_mux::import::Avc3,
    angle_rx: &mut tokio::sync::watch::Receiver<usize>,
    current_angle: &mut usize,
    pts_offset: i64,
) -> Result<i64> {
    let mut input = ffmpeg_next::format::input(path).context("failed to open input file")?;

    let video_stream = input
        .streams()
        .best(ffmpeg_next::media::Type::Video)
        .context("no video stream found")?;

    let stream_index = video_stream.index();
    let time_base = video_stream.time_base();
    let params = video_stream.parameters();

    // Initialize the HD (Avc1) track from the container's extradata.
    let extradata = unsafe { (*params.as_ptr()).extradata };
    let extradata_size = unsafe { (*params.as_ptr()).extradata_size } as usize;
    anyhow::ensure!(!extradata.is_null() && extradata_size > 0, "missing H.264 extradata");
    let avcc = unsafe { std::slice::from_raw_parts(extradata, extradata_size) };
    hd.initialize(&mut &*avcc)?;

    // Set up decoder for the 240p transcode path.
    let dec_ctx = ffmpeg_next::codec::Context::from_parameters(params)?;
    let mut decoder = dec_ctx.decoder().video()?;

    // 240p encoder.
    let enc_codec = ffmpeg_next::encoder::find(ffmpeg_next::codec::Id::H264)
        .context("H.264 encoder not found")?;
    let enc_ctx = ffmpeg_next::codec::Context::new_with_codec(enc_codec);
    let mut enc = enc_ctx.encoder().video()?;
    enc.set_width(426);
    enc.set_height(240);
    enc.set_format(ffmpeg_next::format::Pixel::YUV420P);
    enc.set_time_base(ffmpeg_next::Rational::new(1, 30));
    enc.set_frame_rate(Some(ffmpeg_next::Rational::new(30, 1)));
    enc.set_bit_rate(300_000);
    enc.set_gop(60);

    let mut opts = ffmpeg_next::Dictionary::new();
    opts.set("preset", "ultrafast");
    opts.set("tune", "zerolatency");
    let mut encoder = enc.open_with(opts)?;

    let mut scaler = ffmpeg_next::software::scaling::Context::get(
        decoder.format(),
        decoder.width(),
        decoder.height(),
        ffmpeg_next::format::Pixel::YUV420P,
        426,
        240,
        ffmpeg_next::software::scaling::Flags::BILINEAR,
    )?;

    let mut frame_count: u64 = 0;
    let mut last_pts: i64 = pts_offset;
    let wall_start = std::time::Instant::now();
    let pts_start = pts_offset;

    for (stream, packet) in input.packets() {
        if stream.index() != stream_index {
            continue;
        }

        if angle_rx.has_changed().unwrap_or(false) {
            *current_angle = *angle_rx.borrow_and_update();
            tracing::info!(angle = *current_angle, "angle switch detected");
            return Ok(last_pts);
        }

        let input_pts = packet.pts().unwrap_or(frame_count as i64);
        let pts_micros = rescale_to_micros(input_pts, time_base);
        let adjusted_pts = pts_micros + pts_offset;

        // Pace to real-time.
        let target = std::time::Duration::from_micros((adjusted_pts - pts_start).max(0) as u64);
        let elapsed = wall_start.elapsed();
        if target > elapsed {
            std::thread::sleep(target - elapsed);
        }

        let ts = hang::container::Timestamp::from_micros(adjusted_pts.max(0) as u64)
            .context("timestamp overflow")?;

        // HD: pass AVCC packet directly to Avc1.
        if let Some(data) = packet.data() {
            hd.decode(&mut &*data, Some(ts))?;
        }

        // 240p: decode → scale → burn label → encode → feed to Avc3.
        decoder.send_packet(&packet)?;
        let mut decoded = ffmpeg_next::frame::Video::empty();
        while decoder.receive_frame(&mut decoded).is_ok() {
            let mut yuv = ffmpeg_next::frame::Video::empty();
            scaler.run(&decoded, &mut yuv)?;
            burn_label(&mut yuv);
            yuv.set_pts(Some(frame_count as i64));

            if frame_count == 0 {
                yuv.set_kind(ffmpeg_next::picture::Type::I);
            }

            encoder.send_frame(&yuv)?;
            drain_to_avc3(&mut encoder, preview, ts)?;
            frame_count += 1;
        }

        last_pts = adjusted_pts;
    }

    // Flush decoder + encoder for preview.
    decoder.send_eof()?;
    let mut decoded = ffmpeg_next::frame::Video::empty();
    while decoder.receive_frame(&mut decoded).is_ok() {
        let mut yuv = ffmpeg_next::frame::Video::empty();
        scaler.run(&decoded, &mut yuv)?;
        yuv.set_pts(Some(frame_count as i64));
        encoder.send_frame(&yuv)?;
        let ts = hang::container::Timestamp::from_micros(last_pts.max(0) as u64)
            .context("timestamp")?;
        drain_to_avc3(&mut encoder, preview, ts)?;
        frame_count += 1;
    }

    encoder.send_eof()?;
    let ts =
        hang::container::Timestamp::from_micros(last_pts.max(0) as u64).context("timestamp")?;
    drain_to_avc3(&mut encoder, preview, ts)?;

    Ok(last_pts)
}

/// Drain encoded packets from the encoder and feed them to Avc3 as Annex B frames.
fn drain_to_avc3(
    encoder: &mut ffmpeg_next::encoder::video::Video,
    avc3: &mut moq_mux::import::Avc3,
    pts: hang::container::Timestamp,
) -> Result<()> {
    let mut pkt = ffmpeg_next::Packet::empty();
    while encoder.receive_packet(&mut pkt).is_ok() {
        let data = pkt.data().context("empty encoded packet")?;
        // libx264 with zerolatency outputs Annex B (start code prefixed).
        avc3.decode_frame(&mut &*data, Some(pts))?;
    }
    Ok(())
}

fn rescale_to_micros(pts: i64, time_base: ffmpeg_next::Rational) -> i64 {
    pts * time_base.numerator() as i64 * 1_000_000 / time_base.denominator() as i64
}

/// Burn a "240p" label in the bottom-right corner of a YUV420P frame.
fn burn_label(frame: &mut ffmpeg_next::frame::Video) {
    let w = frame.width() as usize;
    let h = frame.height() as usize;
    let scale = 2usize;

    // 5-wide glyphs, 7 rows each.
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

    // Dark background box.
    for y in y0..y0 + box_h {
        for x in x0..x0 + box_w {
            if x < w && y < h {
                y_data[y * y_stride + x] = 30;
            }
        }
    }

    // Draw glyphs at 2x scale.
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
