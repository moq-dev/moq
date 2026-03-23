use std::path::PathBuf;

use anyhow::{Context, Result};

/// Runs the video encode pipeline: decode from file, encode to H.264, publish to track.
///
/// This runs on a blocking thread since ffmpeg-next is synchronous.
pub fn run_video_pipeline(
    angles: Vec<PathBuf>,
    mut producer: hang::container::OrderedProducer,
    mut angle_rx: tokio::sync::watch::Receiver<usize>,
) -> Result<()> {
    ffmpeg_next::init().context("failed to init ffmpeg")?;

    let mut current_angle = 1usize;
    let mut pts_offset: i64 = 0;

    loop {
        let angle_index = current_angle.saturating_sub(1).min(angles.len() - 1);
        let path = &angles[angle_index];
        tracing::info!(?path, angle = current_angle, "opening video file");

        match encode_from_file(
            path,
            &mut producer,
            &mut angle_rx,
            &mut current_angle,
            pts_offset,
        ) {
            Ok(file_last_pts) => {
                // File ended (looped or angle switch), update offset for continuous timestamps.
                pts_offset = file_last_pts + 33_333; // ~one frame at 30fps in microseconds
            }
            Err(e) => {
                tracing::error!(error = %e, "video pipeline error");
                return Err(e);
            }
        }
    }
}

fn encode_from_file(
    path: &PathBuf,
    producer: &mut hang::container::OrderedProducer,
    angle_rx: &mut tokio::sync::watch::Receiver<usize>,
    current_angle: &mut usize,
    pts_offset: i64,
) -> Result<i64> {
    let mut input = ffmpeg_next::format::input(path).context("failed to open input file")?;

    let video_stream = input
        .streams()
        .best(ffmpeg_next::media::Type::Video)
        .context("no video stream found")?;

    let video_stream_index = video_stream.index();
    let time_base = video_stream.time_base();
    let decoder_params = video_stream.parameters();

    let decoder_context = ffmpeg_next::codec::Context::from_parameters(decoder_params)?;
    let mut decoder = decoder_context.decoder().video()?;

    // Set up encoder.
    let encoder_codec = ffmpeg_next::encoder::find(ffmpeg_next::codec::Id::H264)
        .context("H.264 encoder not found")?;

    let encoder_context = ffmpeg_next::codec::Context::new_with_codec(encoder_codec);
    let mut encoder = encoder_context.encoder().video()?;

    encoder.set_width(decoder.width());
    encoder.set_height(decoder.height());
    encoder.set_format(ffmpeg_next::format::Pixel::YUV420P);
    encoder.set_time_base(ffmpeg_next::Rational::new(1, 30));
    encoder.set_frame_rate(Some(ffmpeg_next::Rational::new(30, 1)));
    encoder.set_bit_rate(2_000_000);
    encoder.set_gop(60); // Keyframe every 2 seconds at 30fps.

    // Try to set low-latency options.
    let mut opts = ffmpeg_next::Dictionary::new();
    opts.set("preset", "ultrafast");
    opts.set("tune", "zerolatency");

    let mut encoder = encoder.open_with(opts)?;

    // Set up scaler for pixel format conversion if needed.
    let mut scaler = ffmpeg_next::software::scaling::Context::get(
        decoder.format(),
        decoder.width(),
        decoder.height(),
        ffmpeg_next::format::Pixel::YUV420P,
        decoder.width(),
        decoder.height(),
        ffmpeg_next::software::scaling::Flags::BILINEAR,
    )?;

    let mut frame_count: u64 = 0;
    let mut last_pts_out: i64 = pts_offset;

    for (stream_obj, packet) in input.packets() {
        if stream_obj.index() != video_stream_index {
            continue;
        }

        // Check for angle switch.
        if angle_rx.has_changed().unwrap_or(false) {
            *current_angle = *angle_rx.borrow_and_update();
            tracing::info!(angle = *current_angle, "angle switch detected mid-file");
            return Ok(last_pts_out);
        }

        decoder.send_packet(&packet)?;

        let mut decoded_frame = ffmpeg_next::frame::Video::empty();
        while decoder.receive_frame(&mut decoded_frame).is_ok() {
            // Scale to YUV420P if needed.
            let mut yuv_frame = ffmpeg_next::frame::Video::empty();
            scaler.run(&decoded_frame, &mut yuv_frame)?;

            // Compute PTS with offset for continuous timestamps across angle switches.
            let input_pts = decoded_frame.pts().unwrap_or(frame_count as i64);
            let pts_micros = rescale_to_micros(input_pts, time_base);
            let adjusted_pts = pts_micros + pts_offset;

            yuv_frame.set_pts(Some(frame_count as i64));

            // Force keyframe at the start of each file/angle switch.
            if frame_count == 0 {
                yuv_frame.set_kind(ffmpeg_next::picture::Type::I);
            }

            encoder.send_frame(&yuv_frame)?;

            let mut encoded_packet = ffmpeg_next::Packet::empty();
            while encoder.receive_packet(&mut encoded_packet).is_ok() {
                let data = encoded_packet.data().context("empty encoded packet")?;
                let is_key = encoded_packet.is_key();

                let timestamp = hang::container::Timestamp::from_micros(adjusted_pts.max(0) as u64)
                    .context("timestamp overflow")?;
                let frame = hang::container::Frame {
                    timestamp,
                    payload: data.to_vec().into(),
                };

                if is_key {
                    producer.keyframe()?;
                }
                producer.write(frame)?;

                last_pts_out = adjusted_pts;
            }

            frame_count += 1;
        }
    }

    // Flush the decoder.
    decoder.send_eof()?;
    let mut decoded_frame = ffmpeg_next::frame::Video::empty();
    while decoder.receive_frame(&mut decoded_frame).is_ok() {
        let mut yuv_frame = ffmpeg_next::frame::Video::empty();
        scaler.run(&decoded_frame, &mut yuv_frame)?;
        yuv_frame.set_pts(Some(frame_count as i64));

        encoder.send_frame(&yuv_frame)?;

        let mut encoded_packet = ffmpeg_next::Packet::empty();
        while encoder.receive_packet(&mut encoded_packet).is_ok() {
            let data = encoded_packet.data().context("empty encoded packet")?;
            let is_key = encoded_packet.is_key();

            let pts_micros = last_pts_out + 33_333;
            let timestamp = hang::container::Timestamp::from_micros(pts_micros.max(0) as u64)
                .context("timestamp overflow")?;
            let frame = hang::container::Frame {
                timestamp,
                payload: data.to_vec().into(),
            };

            if is_key {
                producer.keyframe()?;
            }
            producer.write(frame)?;

            last_pts_out = pts_micros;
        }

        frame_count += 1;
    }

    // Flush the encoder.
    encoder.send_eof()?;
    let mut encoded_packet = ffmpeg_next::Packet::empty();
    while encoder.receive_packet(&mut encoded_packet).is_ok() {
        let data = encoded_packet.data().context("empty encoded packet")?;
        let is_key = encoded_packet.is_key();

        let pts_micros = last_pts_out + 33_333;
        let timestamp = hang::container::Timestamp::from_micros(pts_micros.max(0) as u64)
            .context("timestamp overflow")?;
        let frame = hang::container::Frame {
            timestamp,
            payload: data.to_vec().into(),
        };

        if is_key {
            producer.keyframe()?;
        }
        producer.write(frame)?;

        last_pts_out = pts_micros;
    }

    Ok(last_pts_out)
}

fn rescale_to_micros(pts: i64, time_base: ffmpeg_next::Rational) -> i64 {
    pts * time_base.numerator() as i64 * 1_000_000 / time_base.denominator() as i64
}
