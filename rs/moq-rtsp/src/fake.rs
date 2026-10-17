//! A scripted RTSP server for tests: answers DESCRIBE, SETUP, and PLAY, then sends the RTP a
//! test writes over the TCP-interleaved channels.
//!
//! Behind the `test-support` feature, so downstream tests such as `moq-cli`'s can drive it.

use std::sync::Arc;

use base64::Engine;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::net::tcp::OwnedWriteHalf;
use tokio::sync::Mutex;

/// A real baseline 320x240 SPS from x264.
pub const H264_SPS: &[u8] = &[
	0x67, 0x42, 0xc0, 0x0d, 0xd9, 0x01, 0x41, 0xfb, 0x01, 0x10, 0x00, 0x00, 0x03, 0x00, 0x10, 0x00, 0x00, 0x03, 0x03,
	0xc0, 0xf1, 0x42, 0xa4, 0x80,
];
/// The PPS matching [`H264_SPS`].
pub const H264_PPS: &[u8] = &[0x68, 0xcb, 0x83, 0xcb, 0x20];
/// An IDR slice; its contents are never decoded.
pub const H264_IDR: &[u8] = &[0x65, 0x88, 0x84, 0x21, 0xa0];
/// A recovery-point SEI: the non-IDR I-picture after it is an open-GOP keyframe.
pub const H264_RECOVERY: &[u8] = &[0x06, 0x06, 0x02, 0x00, 0x40, 0x80];
/// A non-IDR slice.
pub const H264_SLICE: &[u8] = &[0x41, 0x9a, 0x02, 0x04, 0x80];

/// A real 320x240 Main profile VPS from x265.
pub const H265_VPS: &[u8] = &[
	0x40, 0x01, 0x0c, 0x01, 0xff, 0xff, 0x04, 0x08, 0x00, 0x00, 0x03, 0x00, 0x9e, 0x08, 0x00, 0x00, 0x03, 0x00, 0x00,
	0x3c, 0x95, 0x98, 0x09,
];
/// The SPS matching [`H265_VPS`].
pub const H265_SPS: &[u8] = &[
	0x42, 0x01, 0x01, 0x04, 0x08, 0x00, 0x00, 0x03, 0x00, 0x9e, 0x08, 0x00, 0x00, 0x03, 0x00, 0x00, 0x3c, 0x90, 0x01,
	0x41, 0x01, 0xe2, 0xcb, 0x2b, 0x34, 0x92, 0x65, 0x78, 0x0b, 0x70, 0x20, 0x20, 0x00, 0x40, 0x00, 0x00, 0x03, 0x00,
	0x40, 0x00, 0x00, 0x03, 0x02, 0x82,
];
/// The PPS matching [`H265_VPS`].
pub const H265_PPS: &[u8] = &[0x44, 0x01, 0xc1, 0x72, 0x86, 0x0c, 0x46, 0x24];
/// An IDR_W_RADL slice; its contents are never decoded.
pub const H265_IDR: &[u8] = &[0x26, 0x01, 0xaf, 0x0c, 0x80];

/// A CRA slice, the open-GOP keyframe x265 emits by default; its contents are never decoded.
pub const H265_CRA: &[u8] = &[0x2a, 0x01, 0xaf, 0x0c, 0x80];

/// A RASL slice, a leading picture of a CRA, displayed before it.
pub const H265_RASL: &[u8] = &[0x10, 0x01, 0xaf, 0x0c, 0x80];
/// A TRAIL_R slice.
pub const H265_TRAIL: &[u8] = &[0x02, 0x01, 0xaf, 0x0c, 0x80];

/// AAC-LC, 48 kHz, stereo.
pub const AAC_CONFIG: &[u8] = &[0x11, 0x90];

/// One stream the server describes.
#[derive(Clone, Copy, Debug)]
pub enum Media {
	/// H.264 at 90 kHz.
	H264 {
		/// Whether the SDP advertises the parameter sets.
		sprop: bool,
	},
	/// H.265 at 90 kHz, parameter sets in the SDP.
	H265,
	/// AAC-LC at 48 kHz in RFC 3640 AAC-hbr mode.
	Aac,
	/// G.711 µ-law at 8 kHz, which the ingest skips.
	Pcmu,
}

impl Media {
	fn sdp(&self, track: usize) -> String {
		let b64 = |data: &[u8]| base64::engine::general_purpose::STANDARD.encode(data);
		let pt = 96 + track;
		let body = match self {
			Self::H264 { sprop } => {
				let sprop = match sprop {
					true => format!(";sprop-parameter-sets={},{}", b64(H264_SPS), b64(H264_PPS)),
					false => String::new(),
				};
				format!(
					"m=video 0 RTP/AVP {pt}\r\na=rtpmap:{pt} H264/90000\r\na=fmtp:{pt} packetization-mode=1;profile-level-id=42c00d{sprop}\r\n"
				)
			}
			Self::H265 => format!(
				"m=video 0 RTP/AVP {pt}\r\na=rtpmap:{pt} H265/90000\r\na=fmtp:{pt} sprop-vps={};sprop-sps={};sprop-pps={}\r\n",
				b64(H265_VPS),
				b64(H265_SPS),
				b64(H265_PPS)
			),
			Self::Aac => format!(
				"m=audio 0 RTP/AVP {pt}\r\na=rtpmap:{pt} mpeg4-generic/48000/2\r\na=fmtp:{pt} streamtype=5;profile-level-id=1;mode=AAC-hbr;sizelength=13;indexlength=3;indexdeltalength=3;config=1190\r\n"
			),
			Self::Pcmu => "m=audio 0 RTP/AVP 0\r\na=rtpmap:0 PCMU/8000\r\n".to_string(),
		};
		format!("{body}a=control:trackID={track}\r\n")
	}

	fn payload_type(&self, track: usize) -> u8 {
		match self {
			Self::Pcmu => 0,
			_ => 96 + track as u8,
		}
	}
}

/// A listener serving one scripted presentation on an ephemeral loopback port.
pub struct Server {
	listener: TcpListener,
	media: Vec<Media>,
	url: url::Url,
	/// The `Authorization` header every request must carry, if any.
	auth: Option<String>,
	/// Whether every request is refused with 403 Forbidden.
	forbidden: bool,
}

impl Server {
	/// Bind a server describing `media`, in order, as tracks `0..`.
	pub async fn bind(media: Vec<Media>) -> Self {
		let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
		let addr = listener.local_addr().expect("local addr");
		let url = format!("rtsp://{addr}/stream").parse().expect("url");
		Self {
			listener,
			media,
			url,
			auth: None,
			forbidden: false,
		}
	}

	/// Refuse every request with 403 Forbidden, as for credentials without access to the stream.
	pub fn forbidden(mut self) -> Self {
		self.forbidden = true;
		self
	}

	/// Challenge every request for HTTP Basic credentials.
	pub fn with_basic_auth(mut self, username: &str, password: &str) -> Self {
		let token = base64::engine::general_purpose::STANDARD.encode(format!("{username}:{password}"));
		self.auth = Some(format!("Basic {token}"));
		self
	}

	/// The URL a client dials.
	pub fn url(&self) -> url::Url {
		self.url.clone()
	}

	/// Accept the next session and answer it through PLAY, starting each track's RTP clock at
	/// `rtptime`.
	pub async fn accept(&self, rtptime: u32) -> Session {
		let (socket, _) = self.listener.accept().await.expect("accept");
		let (read, write) = socket.into_split();
		let mut read = BufReader::new(read);
		let write = Arc::new(Mutex::new(write));
		let mut channels = vec![None; self.media.len()];

		loop {
			let request = read_request(&mut read).await.expect("a request before PLAY");
			let mut headers = vec![format!("CSeq: {}", request.header("cseq").unwrap_or("0"))];
			if self.forbidden {
				respond(&write, "403 Forbidden", &headers, "").await;
				continue;
			}
			if let Some(auth) = &self.auth
				&& request.header("authorization") != Some(auth.as_str())
			{
				headers.push("WWW-Authenticate: Basic realm=\"fake\"".into());
				respond(&write, "401 Unauthorized", &headers, "").await;
				continue;
			}
			let mut body = String::new();
			match request.method.as_str() {
				"OPTIONS" => headers.push("Public: DESCRIBE, SETUP, PLAY, TEARDOWN, GET_PARAMETER".into()),
				"DESCRIBE" => {
					body = format!(
						"v=0\r\no=- 0 0 IN IP4 127.0.0.1\r\ns=fake\r\nt=0 0\r\na=control:*\r\n{}",
						self.media
							.iter()
							.enumerate()
							.map(|(track, media)| media.sdp(track))
							.collect::<String>()
					);
					headers.push(format!("Content-Base: {}/", self.url));
					headers.push("Content-Type: application/sdp".into());
				}
				"SETUP" => {
					let track: usize = request
						.uri
						.rsplit_once("trackID=")
						.and_then(|(_, track)| track.parse().ok())
						.expect("SETUP names a track");
					let transport = request.header("transport").expect("SETUP carries a transport");
					let interleaved = transport
						.split(';')
						.find_map(|part| part.strip_prefix("interleaved="))
						.expect("TCP interleaved transport");
					let channel: u8 = interleaved.split('-').next().unwrap().parse().unwrap();
					channels[track] = Some(channel);
					headers.push(format!("Transport: RTP/AVP/TCP;unicast;interleaved={interleaved}"));
					headers.push("Session: 12345678;timeout=60".into());
				}
				"PLAY" => {
					let info: Vec<String> = channels
						.iter()
						.enumerate()
						.filter(|(_, channel)| channel.is_some())
						.map(|(track, _)| format!("url={}/trackID={track};seq=0;rtptime={rtptime}", self.url))
						.collect();
					headers.push("Session: 12345678".into());
					headers.push(format!("RTP-Info: {}", info.join(",")));
				}
				method => panic!("unexpected {method} before PLAY"),
			}
			respond(&write, "200 OK", &headers, &body).await;
			if request.method == "PLAY" {
				break;
			}
		}

		// Answer keepalives and the TEARDOWN until the client hangs up.
		let responder = write.clone();
		tokio::spawn(async move {
			while let Some(request) = read_request(&mut read).await {
				let cseq = format!("CSeq: {}", request.header("cseq").unwrap_or("0"));
				respond(&responder, "200 OK", &[cseq, "Session: 12345678".into()], "").await;
			}
		});

		Session {
			write,
			media: self.media.clone(),
			channels,
			seq: vec![0; self.media.len()],
		}
	}
}

/// A playing session: the test writes its RTP.
pub struct Session {
	write: Arc<Mutex<OwnedWriteHalf>>,
	media: Vec<Media>,
	channels: Vec<Option<u8>>,
	/// Each track's next RTP sequence number.
	seq: Vec<u16>,
}

impl Session {
	/// Send one access unit on `track`, one RTP packet per NAL (single NAL unit mode).
	pub async fn video(&mut self, track: usize, rtptime: u32, nals: &[&[u8]]) {
		for (index, nal) in nals.iter().enumerate() {
			self.rtp(track, rtptime, index + 1 == nals.len(), nal).await;
		}
	}

	/// Send one AAC access unit on `track` in a single AAC-hbr packet.
	pub async fn aac(&mut self, track: usize, rtptime: u32, frame: &[u8]) {
		let mut payload = vec![0x00, 0x10];
		payload.extend_from_slice(&((frame.len() as u16) << 3).to_be_bytes());
		payload.extend_from_slice(frame);
		self.rtp(track, rtptime, true, &payload).await;
	}

	async fn rtp(&mut self, track: usize, rtptime: u32, marker: bool, payload: &[u8]) {
		let channel = self.channels[track].expect("track was set up");
		let pt = self.media[track].payload_type(track);
		let mut packet = vec![0x80, pt | if marker { 0x80 } else { 0 }];
		packet.extend_from_slice(&self.seq[track].to_be_bytes());
		packet.extend_from_slice(&rtptime.to_be_bytes());
		packet.extend_from_slice(&(0x1000 + track as u32).to_be_bytes());
		packet.extend_from_slice(payload);
		self.seq[track] = self.seq[track].wrapping_add(1);

		let mut frame = vec![b'$', channel];
		frame.extend_from_slice(&(packet.len() as u16).to_be_bytes());
		frame.extend_from_slice(&packet);
		// A client that already hung up is the test's to notice, not the writer's.
		let _ = self.write.lock().await.write_all(&frame).await;
	}

	/// Skip `packets` of `track`'s RTP sequence numbers, as if the network lost them.
	pub fn lose(&mut self, track: usize, packets: u16) {
		self.seq[track] = self.seq[track].wrapping_add(packets);
	}

	/// Send an RTCP sender report for `track`, which carries no media.
	pub async fn rtcp(&mut self, track: usize, rtptime: u32) {
		let channel = self.channels[track].expect("track was set up") + 1;
		// V=2, PT=200 (SR), length 6 (32-bit words minus one), then SSRC, NTP, RTP time, counts.
		let mut packet = vec![0x80, 200, 0, 6];
		packet.extend_from_slice(&(0x1000 + track as u32).to_be_bytes());
		packet.extend_from_slice(&[0; 8]);
		packet.extend_from_slice(&rtptime.to_be_bytes());
		packet.extend_from_slice(&[0; 8]);

		let mut frame = vec![b'$', channel];
		frame.extend_from_slice(&(packet.len() as u16).to_be_bytes());
		frame.extend_from_slice(&packet);
		let _ = self.write.lock().await.write_all(&frame).await;
	}

	/// Hang up, ending the session.
	pub async fn close(self) {
		let _ = self.write.lock().await.shutdown().await;
	}
}

struct Request {
	method: String,
	uri: String,
	headers: Vec<(String, String)>,
}

impl Request {
	fn header(&self, name: &str) -> Option<&str> {
		self.headers
			.iter()
			.find(|(key, _)| key.eq_ignore_ascii_case(name))
			.map(|(_, value)| value.as_str())
	}
}

/// Read one request, or `None` once the client hangs up.
async fn read_request(read: &mut BufReader<tokio::net::tcp::OwnedReadHalf>) -> Option<Request> {
	let mut line = String::new();
	// Skip blank lines between requests.
	while line.trim().is_empty() {
		line.clear();
		if read.read_line(&mut line).await.ok()? == 0 {
			return None;
		}
	}
	let mut parts = line.split_whitespace();
	let method = parts.next()?.to_string();
	let uri = parts.next()?.to_string();

	let mut headers = Vec::new();
	loop {
		let mut line = String::new();
		if read.read_line(&mut line).await.ok()? == 0 {
			return None;
		}
		let line = line.trim_end();
		if line.is_empty() {
			break;
		}
		let (key, value) = line.split_once(':')?;
		headers.push((key.trim().to_string(), value.trim().to_string()));
	}

	let request = Request { method, uri, headers };
	if let Some(length) = request.header("content-length") {
		let mut body = vec![0; length.parse().ok()?];
		read.read_exact(&mut body).await.ok()?;
	}
	Some(request)
}

async fn respond(write: &Mutex<OwnedWriteHalf>, status: &str, headers: &[String], body: &str) {
	let mut response = format!("RTSP/1.0 {status}\r\n");
	for header in headers {
		response.push_str(header);
		response.push_str("\r\n");
	}
	if !body.is_empty() {
		response.push_str(&format!("Content-Length: {}\r\n", body.len()));
	}
	response.push_str("\r\n");
	response.push_str(body);
	let _ = write.lock().await.write_all(response.as_bytes()).await;
}
