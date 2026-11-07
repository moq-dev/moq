use std::net::{IpAddr, SocketAddr};
use std::str::FromStr;

use crate::crypto::Fingerprint;
use crate::io::TcpType;
use crate::rtp_::{Direction, Extension, Frequency, Mid, Pt, SessionId, Ssrc};
use crate::{Candidate, CandidateKind};
use combine::error::*;
use combine::parser::char::*;
use combine::parser::combinator::*;
use combine::stream::StreamErrorFor;
use combine::*;
use combine::{ParseError, Parser, Stream};

use super::data::*;

/// Creates a parser of SDP
pub fn sdp_parser<Input>() -> impl Parser<Input, Output = Sdp>
where
    Input: Stream<Token = char>,
    Input::Error: ParseError<Input::Token, Input::Range, Input::Position>,
{
    (session_parser(), many::<Vec<_>, _, _>(media_parser())).map(|(session, media)| Sdp {
        session,
        media_lines: media,
    })
}

// /////////////////////////////////////////////////// Session description

/// 1. First line must be v=0
/// 2. The second SDP line MUST be an "o=" line The sess-id MUST be representable by a 64-bit signed
///    integer, and the initial value MUST be less than (2**62)-1
/// 3. Third line is the session name. The "s=" field MUST NOT be empty and SHOULD contain
///    ISO 10646 characters (RFC 8866 section 5.3).
///
/// Session is over when we find a "t=" line MUST be added, both <start-time> and <stop-time>
///    SHOULD be set to zero, e.g. "t=0 0".
pub fn session_parser<Input>() -> impl Parser<Input, Output = Session>
where
    Input: Stream<Token = char>,
    Input::Error: ParseError<Input::Token, Input::Range, Input::Position>,
{
    (
        typed_line('v', token('0')), // v=0
        originator_line(),           // o=- 6564425948916445306 2 IN IP4 127.0.0.1
        // The "s=" field MUST NOT be empty (RFC 8866 section 5.3)
        typed_line('s', any_value()),
        many::<Vec<_>, _, _>(ignored_session_line()),
        optional(bandwidth_line()),                         // b=CT:1234
        typed_line('t', string("0 0")),                     // t=0 0
        many::<Vec<_>, _, _>(typed_line('r', any_value())), // r should never appear
        //
        many::<Vec<_>, _, _>(session_attribute_line()),
    )
        .map(|(_, id, _, _, bw, _, _, attrs)| Session { id, bw, attrs })
}

/// `o=<username> <sess-id> <sess-version> <nettype> <addrtype> <unicast-address>`
fn originator_line<Input>() -> impl Parser<Input, Output = SessionId>
where
    Input: Stream<Token = char>,
    Input::Error: ParseError<Input::Token, Input::Range, Input::Position>,
{
    let session_string = typed_line(
        'o',
        // o=- 6564425948916445306 2 IN IP4 127.0.0.1
        (
            not_sp(),
            token(' '),
            many1::<String, _, _>(digit()),
            token(' '),
            any_value(),
        )
            .map(|(_, _, sess, _, _)| sess),
    );
    from_str(session_string).map(|x: u64| -> SessionId { SessionId::from(x) })
}

/// `b=<bwtype>:<bandwidth>`
fn bandwidth_line<Input>() -> impl Parser<Input, Output = Bandwidth>
where
    Input: Stream<Token = char>,
    Input::Error: ParseError<Input::Token, Input::Range, Input::Position>,
{
    typed_line('b', (many1(satisfy(|c| c != ':')), token(':'), any_value()))
        .map(|(typ, _, val)| Bandwidth { typ, val })
}

/// An a= line that with value like: `a=<attribute>:<value>`.
fn attribute_line<Input, Pval, Out>(
    attribute: &'static str,
    val: Pval,
) -> impl Parser<Input, Output = Out>
where
    Input: Stream<Token = char>,
    Pval: Parser<Input, Output = Out>,
    Input::Error: ParseError<Input::Token, Input::Range, Input::Position>,
{
    typed_line('a', (string(attribute), token(':'), val)).map(|(_, _, val)| val)
}

/// An a= line that has no value like: `a=ice-lite`.
fn attribute_line_flag<Input>(attribute: &'static str) -> impl Parser<Input, Output = ()>
where
    Input: Stream<Token = char>,
    Input::Error: ParseError<Input::Token, Input::Range, Input::Position>,
{
    typed_line('a', (string(attribute)).map(|_| ()))
}

/// a=foo:bar lines belonging before the first m= line
fn session_attribute_line<Input>() -> impl Parser<Input, Output = SessionAttribute>
where
    Input: Stream<Token = char>,
    Input::Error: ParseError<Input::Token, Input::Range, Input::Position>,
{
    // a=group:BUNDLE 1 2
    // a=group:LS 1 2
    let group = attribute_line(
        "group",
        (
            not_sp(),
            many::<Vec<_>, _, _>(attempt(
                (token(' '), not_sp()).map(|(_, m)| Mid::from(m.as_str())),
            )),
            optional(token(' ')),
        ),
    )
    .map(|(typ, mids, _)| SessionAttribute::Group { typ, mids });

    // a=msid-semantic: WMS 9ce81ef6-c7cb-4da5-8f5f-3e7cc9b5f9b0
    let msid_semantic = attribute_line(
        "msid-semantic",
        (
            optional(token(' ')),
            not_sp(),
            token(' '),
            sep_by1(not_sp(), token(' ')),
        ),
    )
    .map(
        |(_, semantic, _, stream_ids)| SessionAttribute::MsidSemantic {
            semantic,
            stream_ids,
        },
    );

    // a=ice-lite
    let ice_lite = attribute_line_flag("ice-lite").map(|_| SessionAttribute::IceLite);

    // a=ice-ufrag:IdNYTNL1fjvjyEzL
    let ice_ufrag = attribute_line("ice-ufrag", any_value()).map(SessionAttribute::IceUfrag);

    // a=ice-pwd:4d64pT3T1xfwbZvi9fQKjoPb
    let ice_pwd = attribute_line("ice-pwd", any_value()).map(SessionAttribute::IcePwd);

    // a=ice-options:trickle
    let ice_opt = attribute_line("ice-options", any_value()).map(SessionAttribute::IceOptions);

    // a=fingerprint:sha-256 45:AD:5C:82:F8:BE:B5:2A:D1:74:A6:16:D0:50:CD:86:9C:97:9D:BD:06:\
    // 8C:C9:85:C9:CD:AB:2B:A8:56:03:CD
    // "sha-1" / "sha-224" / "sha-256" /
    // "sha-384" / "sha-512" /
    // "md5" / "md2"
    let hex_byte = count_min_max(2, 2, hex_digit()).and_then(|x: String| {
        u8::from_str_radix(&x, 16).map_err(StreamErrorFor::<Input>::message_format)
    });
    let finger = attribute_line(
        "fingerprint",
        (not_sp(), token(' '), sep_by1(hex_byte, token(':'))),
    )
    .map(|(hash_func, _, bytes)| SessionAttribute::Fingerprint(Fingerprint { hash_func, bytes }));

    let setup_val = choice((
        attempt(string("actpass").map(|_| Setup::ActPass)),
        attempt(string("active").map(|_| Setup::Active)),
        attempt(string("passive").map(|_| Setup::Passive)),
    ));

    // a=setup:actpass
    let setup = attribute_line("setup", setup_val).map(SessionAttribute::Setup);

    // a=candidate
    let cand = candidate_attribute().map(SessionAttribute::Candidate);

    // a=end-of-candidates
    let endof = attribute_line_flag("end-of-candidates").map(|_| SessionAttribute::EndOfCandidates);

    // tls-id
    // identity
    // extmap
    let unused = typed_line('a', any_value()).map(SessionAttribute::Unused);

    choice((
        attempt(group),
        attempt(msid_semantic),
        attempt(ice_lite),
        attempt(ice_ufrag),
        attempt(ice_pwd),
        attempt(ice_opt),
        attempt(finger),
        attempt(setup),
        attempt(cand),
        attempt(endof),
        unused,
    ))
}

/// Parser for candidate, without attribute prefix (a=).
fn candidate<Input>() -> impl Parser<Input, Output = Candidate>
where
    Input: Stream<Token = char>,
    Input::Error: ParseError<Input::Token, Input::Range, Input::Position>,
{
    // Reference: https://datatracker.ietf.org/doc/html/rfc5245#section-15.1
    let port = || {
        not_sp::<Input>().and_then(|s| {
            s.parse::<u16>()
                .map_err(StreamErrorFor::<Input>::message_format)
        })
    };

    let ip_addr = || {
        not_sp().and_then(|s| {
            s.parse::<IpAddr>()
                .map_err(StreamErrorFor::<Input>::message_format)
        })
    };

    let kind = choice((
        string("host").map(|_| CandidateKind::Host),
        string("prflx").map(|_| CandidateKind::PeerReflexive),
        string("srflx").map(|_| CandidateKind::ServerReflexive),
        string("relay").map(|_| CandidateKind::Relayed),
    ));

    (
        string("candidate:").and_then(|s| {
            s.parse::<String>()
                .map_err(StreamErrorFor::<Input>::message_format)
        }),
        not_sp(),
        token(' '),
        not_sp().and_then(|s| {
            s.parse::<u16>()
                .map_err(StreamErrorFor::<Input>::message_format)
        }),
        token(' '),
        not_sp().and_then(|s| {
            s.as_str().try_into().map_err(|_| {
                StreamErrorFor::<Input>::message_format(format!("invalid protocol: {}", s))
            })
        }),
        token(' '),
        not_sp().and_then(|s| {
            s.parse::<u32>()
                .map_err(StreamErrorFor::<Input>::message_format)
        }),
        token(' '),
        ip_addr(),
        token(' '),
        port(),
        string(" typ "),
        kind,
        optional((
            attempt(string(" raddr ")),
            ip_addr(),
            string(" rport "),
            port(),
        )),
        optional((
            attempt(string(" tcptype ")),
            not_sp().and_then(|s| {
                TcpType::from_str(s.as_str()).map_err(StreamErrorFor::<Input>::message_format)
            }),
        )),
        optional((attempt(string(" generation ")), not_sp())),
        optional((attempt(string(" network-id ")), not_sp())),
        optional((attempt(string(" ufrag ")), not_sp())),
        optional((attempt(string(" network-cost ")), not_sp())),
    )
        .map(
            |(
                _,
                found,
                _,
                comp_id,
                _,
                proto,
                _,
                prio,
                _,
                addr,
                _,
                port,
                _,
                kind,
                raddr,   // (" raddr ", addr, " rport ", port)
                tcptype, // (" tcptype ", tcptype)
                _,       // (" generation ", generation)
                _,       // (" network-id ", network_id)
                ufrag,   // (" ufrag ", ufrag)
                _,       // ("network-cost", network_cost)
            )| {
                Candidate::from_parts(
                    found,
                    comp_id,
                    proto,
                    prio, // remote candidates calculate prio on their side
                    SocketAddr::from((addr, port)),
                    kind,
                    raddr.map(|(_, addr, _, port)| SocketAddr::from((addr, port))),
                    tcptype.map(|(_, tcptype)| tcptype),
                    ufrag.map(|(_, u)| u),
                )
            },
        )
}

/// Parser for a=candidate lines.
pub(crate) fn candidate_attribute<Input>() -> impl Parser<Input, Output = Candidate>
where
    Input: Stream<Token = char>,
    Input::Error: ParseError<Input::Token, Input::Range, Input::Position>,
{
    (string("a="), candidate(), line_end()).map(|(_, c, _)| c)
}

/// Session line with a key we ignore (spec says we should validate them, but meh).
fn ignored_session_line<Input>() -> impl Parser<Input, Output = ()>
where
    Input: Stream<Token = char>,
    Input::Error: ParseError<Input::Token, Input::Range, Input::Position>,
{
    // TODO: there must be a better way
    let ignored = choice((
        token('i'),
        token('u'),
        token('e'),
        token('p'),
        token('c'),
        token('z'),
        token('k'),
    ));
    line(ignored, any_value()).map(|_| ())
}

// /////////////////////////////////////////////////// Media description

/// A m= section with attributes, until next m= or EOF
fn media_parser<Input>() -> impl Parser<Input, Output = MediaLine>
where
    Input: Stream<Token = char>,
    Input::Error: ParseError<Input::Token, Input::Range, Input::Position>,
{
    (
        media_line(),
        optional(typed_line('c', any_value())), // c=IN IP4 0.0.0.0
        optional(bandwidth_line()),             // b=AS:2500
        many::<Vec<_>, _, _>(media_attribute_line()),
    )
        .and_then(|((typ, port, proto, pts), _, bw, attrs)| {
            let m = MediaLine {
                typ,
                disabled: port == "0",
                proto,
                pts,
                bw,
                attrs,
            };
            if let Some(err) = m.check_consistent() {
                warn!("{:?}", err);
                return Err(StreamErrorFor::<Input>::message_format(err));
            }
            Ok(m)
        })
}

/// The m= line
// dormammu:
// m=audio 9 UDP/TLS/RTP/SAVPF 111
// m=video 9 UDP/TLS/RTP/SAVPF 96 97 125 107 100 101
// m=application 9 DTLS/SCTP 5000
// chrome:
// m=audio 64205 UDP/TLS/RTP/SAVPF 111
// m=video 53151 UDP/TLS/RTP/SAVPF 96 97 125 107 100 101
// m=application 54055 DTLS/SCTP 5000
//
// newer chrome:
// m=application 9 UDP/DTLS/SCTP webrtc-datachannel
fn media_line<Input>() -> impl Parser<Input, Output = (MediaType, String, Proto, Vec<Pt>)>
where
    Input: Stream<Token = char>,
    Input::Error: ParseError<Input::Token, Input::Range, Input::Position>,
{
    let media_type = choice((
        attempt(string("audio").map(|_| MediaType::Audio)),
        attempt(string("video").map(|_| MediaType::Video)),
        attempt(string("application").map(|_| MediaType::Application)),
        not_sp().map(MediaType::Unknown),
    ));

    let proto_line = choice((
        attempt(string("UDP/TLS/RTP/SAVPF").map(|_| Proto::Srtp)),
        attempt(string("DTLS/SCTP").map(|_| Proto::Sctp)),
        attempt(string("UDP/DTLS/SCTP").map(|_| Proto::Sctp)),
    ));

    let parse_pt = not_sp().and_then(|s| {
        s.parse::<u8>()
            .map(Pt::from)
            .map_err(StreamErrorFor::<Input>::message_format)
    });

    // m=<media> <port> <proto> <fmt> ...
    // <fmt> is: <pt> <pt> <pt> <pt>
    // where <pt> either matches a=rtpmap:<pt> or a=sctpmap:<pt>
    typed_line(
        'm',
        (
            media_type, // type: audio, video etc.
            token(' '),
            not_sp(), // port: just set to 9 or something
            token(' '),
            proto_line, // proto:  or
            token(' '),
            choice((
                attempt(sep_by(parse_pt, token(' '))), // <pt> <pt>
                any_value().map(|_| vec![]),
            )),
        ),
    )
    .map(|(typ, _, port, _, proto, _, pts)| (typ, port, proto, pts))
}

/// a=foo:bar lines belonging before the first m= line
fn media_attribute_line<Input>() -> impl Parser<Input, Output = MediaAttribute>
where
    Input: Stream<Token = char>,
    Input::Error: ParseError<Input::Token, Input::Range, Input::Position>,
{
    // a=rtcp:9 IN IP4 0.0.0.0
    let rtcp = attribute_line("rtcp", any_value()).map(MediaAttribute::Rtcp);

    // a=ice-ufrag:IdNYTNL1fjvjyEzL
    let ice_ufrag = attribute_line("ice-ufrag", any_value()).map(MediaAttribute::IceUfrag);

    // a=ice-pwd:4d64pT3T1xfwbZvi9fQKjoPb
    let ice_pwd = attribute_line("ice-pwd", any_value()).map(MediaAttribute::IcePwd);

    // a=ice-options:trickle
    let ice_opt = attribute_line("ice-options", any_value()).map(MediaAttribute::IceOptions);

    // a=fingerprint:sha-256 45:AD:5C:82:F8:BE:B5:2A:D1:74:A6:16:D0:50:CD:86:9C:97:9D:BD:06:\
    // 8C:C9:85:C9:CD:AB:2B:A8:56:03:CD
    // "sha-1" / "sha-224" / "sha-256" /
    // "sha-384" / "sha-512" /
    // "md5" / "md2"
    let hex_byte = count_min_max(2, 2, hex_digit()).and_then(|x: String| {
        u8::from_str_radix(&x, 16).map_err(StreamErrorFor::<Input>::message_format)
    });
    let finger = attribute_line(
        "fingerprint",
        (not_sp(), token(' '), sep_by1(hex_byte, token(':'))),
    )
    .map(|(hash_func, _, bytes)| MediaAttribute::Fingerprint(Fingerprint { hash_func, bytes }));

    let setup_val = choice((
        attempt(string("actpass").map(|_| Setup::ActPass)),
        attempt(string("active").map(|_| Setup::Active)),
        attempt(string("passive").map(|_| Setup::Passive)),
    ));

    // a=setup:actpass
    let setup = attribute_line("setup", setup_val).map(MediaAttribute::Setup);

    // a=mid:0
    let mid = attribute_line("mid", any_value())
        .map(|m| Mid::from(m.as_str()))
        .map(MediaAttribute::Mid);

    let sctp_port = attribute_line(
        "sctp-port",
        not_sp::<Input>().and_then(|s| {
            s.parse::<u16>()
                .map_err(StreamErrorFor::<Input>::message_format)
        }),
    )
    .map(MediaAttribute::SctpPort);

    let max_message_size = attribute_line(
        "max-message-size",
        not_sp::<Input>().and_then(|s| {
            s.parse::<usize>()
                .map_err(StreamErrorFor::<Input>::message_format)
        }),
    )
    .map(MediaAttribute::MaxMessageSize);

    // a=sctp-init:<base64-encoded SCTP INIT chunk>
    // See draft-hancke-tsvwg-snap (§4.2: base64 defined in RFC 4566, i.e. standard with padding)
    //
    // The base64 value is stored as-is; decoding happens when consumed.
    let sctp_init = attribute_line("sctp-init", any_value()).map(MediaAttribute::SctpInit);

    // a=extmap:1 urn:ietf:params:rtp-hdrext:ssrc-audio-level
    // a=extmap:<value>["/"<direction>] <URI> <extensionattributes>
    let extmap = attribute_line(
        "extmap",
        (
            many1::<String, _, _>(satisfy(|c| c != '/' && c != ' ')).and_then(|s| {
                s.parse::<u8>()
                    .map_err(StreamErrorFor::<Input>::message_format)
            }),
            optional((token('/'), not_sp().map(|d| Direction::from(&d[..])))),
            token(' '),
            not_sp().map(|uri| Extension::from_sdp_uri(&uri)),
            optional((token(' '), any_value())),
        ),
    )
    .map(|(id, _dir_opt, _, ext, _ext_opt)| MediaAttribute::ExtMap { id, ext });

    let direction = choice((
        attempt(attribute_line_flag("recvonly").map(|_| MediaAttribute::RecvOnly)),
        attempt(attribute_line_flag("sendrecv").map(|_| MediaAttribute::SendRecv)),
        attempt(attribute_line_flag("sendonly").map(|_| MediaAttribute::SendOnly)),
        attempt(attribute_line_flag("inactive").map(|_| MediaAttribute::Inactive)),
    ));

    // // a=msid:5UUdwiuY7OML2EkQtF38pJtNP5v7In1LhjEK f78dde68-7055-4e20-bb37-433803dd1ed1
    // // a=msid:- f78dde68-7055-4e20-bb37-433803dd1ed1
    let msid = attribute_line("msid", (not_sp(), token(' '), any_value())).map(
        |(stream_id, _, track_id)| {
            MediaAttribute::Msid(Msid {
                stream_id,
                track_id,
            })
        },
    );

    let rtcpmux = attribute_line_flag("rtcp-mux").map(|_| MediaAttribute::RtcpMux);
    let rtcpmuxonly = attribute_line_flag("rtcp-mux-only").map(|_| MediaAttribute::RtcpMuxOnly);
    let rtcprsize = attribute_line_flag("rtcp-rsize").map(|_| MediaAttribute::RtcpRsize);

    // a=candidate
    let cand = candidate_attribute().map(MediaAttribute::Candidate);

    // a=end-of-candidates
    let endof = attribute_line_flag("end-of-candidates").map(|_| MediaAttribute::EndOfCandidates);

    let pt = || {
        not_sp().and_then(|s| {
            let pt = s
                .parse::<u8>()
                .map_err(StreamErrorFor::<Input>::message_format)?;
            // RTP payload type is 7-bit; reject out-of-range values rather than panicking later
            // when PTs are claimed against a 128-entry table.
            if pt > 127 {
                return Err(StreamErrorFor::<Input>::message_format(format!(
                    "payload type out of range (0-127): {pt}"
                )));
            }
            Ok(Pt::from(pt))
        })
    };

    // a=rtpmap:111 opus/48000/2
    let rtpmap = attribute_line(
        "rtpmap",
        (
            pt(),
            token(' '),
            many1::<String, _, _>(satisfy(|c| c != '/' && c != '\r' && c != '\n')),
            token('/'),
            many1::<String, _, _>(satisfy(|c| c != '/' && c != '\r' && c != '\n')).and_then(|s| {
                s.parse::<Frequency>()
                    .map_err(StreamErrorFor::<Input>::message_format)
            }),
            optional((
                token('/'),
                any_value().and_then(|s| {
                    s.parse::<u8>()
                        .map_err(StreamErrorFor::<Input>::message_format)
                }),
            )), // only audio has the last /2 (channels)
        ),
    )
    .map(|(pt, _, codec, _, clock_rate, opt_channels)| {
        let channels = opt_channels.map(|(_, e)| e);
        MediaAttribute::RtpMap {
            pt,
            value: RtpMap {
                codec: codec.as_str().into(),
                clock_rate,
                channels,
            },
        }
    });

    // a=rtcp-fb:111 transport-cc
    // a=rtcp-fb:111 ccm fir
    // a=rtcp-fb:111 nack
    // a=rtcp-fb:111 nack pli
    let rtcp_fb = attribute_line("rtcp-fb", (pt(), token(' '), any_value()))
        .map(|(pt, _, value)| MediaAttribute::RtcpFb { pt, value });

    // Tolerate horizontal whitespace around `;` and a trailing `;`.
    let fmtp_param = (
        sep_end_by1(attempt(key_val()), attempt((spaces(false), token(';')))),
        spaces(false),
    )
        .map(|(pairs, _)| FormatParam::parse_pairs(pairs));

    // a=fmtp:111 minptime=10; useinbandfec=1
    // a=fmtp:111 minptime=10;useinbandfec=1
    let fmtp1 = attribute_line("fmtp", (pt(), token(' '), fmtp_param))
        .map(|(pt, _, values)| MediaAttribute::Fmtp { pt, values });

    // Parse bare fmtp syntax without assigning codec meaning; rtpmap decides that later.
    let fmtp_pt_num = || {
        many1::<String, _, _>(satisfy(|c| c != '/' && c != '\r' && c != '\n')).and_then(|s| {
            s.parse::<u8>()
                .map_err(StreamErrorFor::<Input>::message_format)
        })
    };
    let fmtp_pt_list = attribute_line(
        "fmtp",
        (
            pt(),
            token(' '),
            fmtp_pt_num(),
            skip_many1((token('/'), fmtp_pt_num())),
        ),
    )
    .map(|(pt, _, primary, _)| MediaAttribute::Fmtp {
        pt,
        values: vec![FormatParam::BarePtList(Pt::from(primary))],
    });

    let fmtp_range = attribute_line(
        "fmtp",
        (
            pt(),
            token(' '),
            string("0-"),
            many1::<String, _, _>(satisfy(|c: char| c.is_ascii_digit())).and_then(|s| {
                s.parse::<u8>()
                    .map_err(StreamErrorFor::<Input>::message_format)
            }),
        ),
    )
    .map(|(pt, _, _, max)| MediaAttribute::Fmtp {
        pt,
        values: vec![FormatParam::BareRange(max)],
    });

    let fmtp_unknown = attribute_line("fmtp", (pt(), optional(token(' ')), optional(any_value())))
        .map(|(pt, _, _)| MediaAttribute::Fmtp {
            pt,
            values: vec![FormatParam::Unknown],
        });

    let fmtp = choice((
        attempt(fmtp1),
        attempt(fmtp_pt_list),
        attempt(fmtp_range),
        attempt(fmtp_unknown),
    ));

    // a=rid:<rid-id> <direction> [pt=<fmt-list>;]<restriction>=<value>
    let rid = attribute_line(
        "rid",
        (
            name().map(RestrictionId::new_active),
            token(' '),
            choice((string("send"), string("recv"))),
            optional((
                token(' '),
                optional((
                    string("pt="),
                    sep_by1::<Vec<Pt>, _, _, _>(pt(), token(',')),
                    // TODO this is not really optional when there is
                    // a restriction part. It means we are incorrectly
                    // allowing this: a=rid:foo send pt=111max-br=64000
                    optional(token(';')),
                )),
                sep_by::<Vec<(String, String)>, _, _, _>(key_val(), token(';')),
            )),
        ),
    )
    .map(|(id, _, direction, x)| {
        let mut pt = vec![];
        let mut restriction = vec![];
        if let Some((_, ps, rs)) = x {
            if let Some((_, ps, _)) = ps {
                pt = ps;
            }
            restriction = rs;
        }

        MediaAttribute::Rid {
            id,
            direction,
            pt,
            restriction,
        }
    });

    let simul1 = |direction: &'static str| {
        (
            string(direction),
            token(' '),
            sep_by1::<Vec<Vec<(String, bool)>>, _, _, _>(
                sep_by1(
                    optional(token('~'))
                        .and(name())
                        .map(|x| (x.1, x.0.is_none())),
                    token(','),
                ),
                token(';'),
            ),
        )
    };

    // Parser guarantee that it's send => recv or recv => send.
    let simul2 = choice((
        (simul1("send"), optional((token(' '), simul1("recv")))),
        (simul1("recv"), optional((token(' '), simul1("send")))),
    ));

    // a=simulcast:<send/recv> <alt A>;<alt B>,<or C> <send/recv> [same]
    let simulcast = attribute_line("simulcast", simul2).map(|(s1, maybe_s2)| {
        let mut send = SimulcastGroups(vec![]);
        let mut recv = SimulcastGroups(vec![]);

        fn to_simul(to: &mut SimulcastGroups, groups: Vec<Vec<(String, bool)>>) {
            for group in groups {
                // TODO: Properly support rid alternatives, for now we ignore any alternatives
                // provided and use the first rid.
                let first = group.into_iter().next();

                if let Some(rid) = first.map(|(rid, active)| RestrictionId::new(rid, active)) {
                    to.0.push(SimulcastLayer {
                        restriction_id: rid,
                        attributes: None,
                    });
                }
            }
        }

        {
            let to = if s1.0 == "send" { &mut send } else { &mut recv };
            to_simul(to, s1.2);
        }

        if let Some(s2) = maybe_s2 {
            let s2 = s2.1;
            let to = if s2.0 == "send" { &mut send } else { &mut recv };
            to_simul(to, s2.2);
        }

        MediaAttribute::Simulcast(Simulcast {
            send,
            recv,
            is_munged: false,
        })
    });

    // a=ssrc-group:FID 1111 2222
    let ssrc_group = attribute_line(
        "ssrc-group",
        (
            not_sp(),
            token(' '),
            sep_by1(
                not_sp().and_then(|s| {
                    s.parse::<u32>()
                        .map(Ssrc::from)
                        .map_err(StreamErrorFor::<Input>::message_format)
                }),
                token(' '),
            ),
        ),
    )
    .map(|(semantics, _, ssrcs)| MediaAttribute::SsrcGroup { semantics, ssrcs });

    // a=ssrc:3948621874 cname:xeXs3aE9AOBn00yJ
    // a=ssrc:3948621874 msid:5UUdwiuY7OML2EkQtF38pJtNP5v7In1LhjEK f78dde68-7055-4e20-bb37-433803dd1ed1
    // a=ssrc:3948621874 mslabel:5UUdwiuY7OML2EkQtF38pJtNP5v7In1LhjEK
    // a=ssrc:3948621874 label:f78dde68-7055-4e20-bb37-433803dd1ed1
    let ssrc = attribute_line(
        "ssrc",
        (
            not_sp().and_then(|s| {
                s.parse::<u32>()
                    .map(Ssrc::from)
                    .map_err(StreamErrorFor::<Input>::message_format)
            }),
            token(' '),
            many1::<String, _, _>(satisfy(|c| c != ':')),
            token(':'),
            any_value(),
        ),
    )
    .map(|(ssrc, _, attr, _, value)| MediaAttribute::Ssrc { ssrc, attr, value });

    let unused = typed_line('a', any_value()).map(MediaAttribute::Unused);

    // Split into two choice() calls because combine's choice macro supports at most 25 parsers.
    choice((
        attempt(choice((
            attempt(ice_ufrag),
            attempt(ice_pwd),
            attempt(ice_opt),
            attempt(finger),
            attempt(setup),
            attempt(mid),
            attempt(sctp_port),
            attempt(max_message_size),
            attempt(sctp_init),
            attempt(extmap),
            attempt(direction),
            attempt(msid),
            attempt(rtcp),
        ))),
        attempt(choice((
            attempt(rtcpmux),
            attempt(rtcpmuxonly),
            attempt(rtcprsize),
            attempt(cand),
            attempt(endof),
            attempt(rtpmap),
            attempt(rtcp_fb),
            attempt(fmtp),
            attempt(rid),
            attempt(simulcast),
            attempt(ssrc_group),
            attempt(ssrc),
        ))),
        unused,
    ))
}

// /////////////////////////////////////////////////// Generic things below

/// A specific line
fn typed_line<Input, Pval, Out>(expected: char, val: Pval) -> impl Parser<Input, Output = Out>
where
    Input: Stream<Token = char>,
    Pval: Parser<Input, Output = Out>,
    Input::Error: ParseError<Input::Token, Input::Range, Input::Position>,
{
    line(token(expected), val)
}

/// A line with some parser for value and parser for type.
fn line<Input, Ptyp, Pval, Out>(typ: Ptyp, val: Pval) -> impl Parser<Input, Output = Out>
where
    Ptyp: Parser<Input, Output = char>,
    Pval: Parser<Input, Output = Out>,
    Input: Stream<Token = char>,
    Input::Error: ParseError<Input::Token, Input::Range, Input::Position>,
{
    attempt((typ, token('='), val, line_end()))
        .map(|(_, _, value, _)| value)
        .message("sdp line")
}

/// alphanumeric name.
fn name<Input>() -> impl Parser<Input, Output = String>
where
    Input: Stream<Token = char>,
    Input::Error: ParseError<Input::Token, Input::Range, Input::Position>,
{
    many1(satisfy(|c: char| c.is_alphanumeric()))
}

/// Not SP, \r or \n
fn not_sp<Input>() -> impl Parser<Input, Output = String>
where
    Input: Stream<Token = char>,
    Input::Error: ParseError<Input::Token, Input::Range, Input::Position>,
{
    many1(satisfy(|c| c != ' ' && c != '\r' && c != '\n'))
}

/// Any value that isn't \r or \n.
fn any_value<Input>() -> impl Parser<Input, Output = String>
where
    Input: Stream<Token = char>,
    Input::Error: ParseError<Input::Token, Input::Range, Input::Position>,
{
    many1(satisfy(|c| c != '\r' && c != '\n'))
}

/// We discovered a stray \n in safari SDP. This line end handles \r\n, \n or EOF.
fn line_end<Input>() -> impl Parser<Input, Output = ()>
where
    Input: Stream<Token = char>,
    Input::Error: ParseError<Input::Token, Input::Range, Input::Position>,
{
    choice((crlf().map(|_| ()), newline().map(|_| ()), eof()))
}

// minptime=10
fn key_val<Input>() -> impl Parser<Input, Output = (String, String)>
where
    Input: Stream<Token = char>,
    Input::Error: ParseError<Input::Token, Input::Range, Input::Position>,
{
    (
        spaces(false),
        many1(satisfy(|c| is_param_char(c, '='))),
        token('='),
        many1(satisfy(|c| is_param_char(c, ';'))),
    )
        .map(|(_, key, _, val)| (key, val))
}

/// Skip spaces and tabs, optionally including CR and LF.
fn spaces<Input>(newline: bool) -> impl Parser<Input, Output = ()>
where
    Input: Stream<Token = char>,
    Input::Error: ParseError<Input::Token, Input::Range, Input::Position>,
{
    skip_many(satisfy(move |c| {
        matches!(c, ' ' | '\t') || (newline && matches!(c, '\r' | '\n'))
    }))
}

// Parameter tokens exclude horizontal whitespace, line endings, and their delimiter.
fn is_param_char(c: char, delimiter: char) -> bool {
    c != delimiter && !matches!(c, ' ' | '\t' | '\r' | '\n')
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::format::Codec;
    use crate::io::Protocol;

    #[test]
    fn spaces_preserve_or_consume_line_endings() {
        for end in ["\r\n", "\n", "\r"] {
            let input = format!(" \t{end}next");
            let remaining = format!("{end}next");
            assert_eq!(
                spaces(false).parse(input.as_str()),
                Ok(((), remaining.as_str()))
            );
            assert_eq!(spaces(true).parse(input.as_str()), Ok(((), "next")));
        }
    }

    #[test]
    fn line_a() {
        assert_eq!(
            line(letter(), any_value()).parse("a=mid:0"),
            Ok(("mid:0".to_string(), ""))
        )
    }

    #[test]
    fn line_end_crlf() {
        assert_eq!(line_end().parse("\r\n"), Ok(((), "")));
    }

    #[test]
    fn line_end_lf() {
        assert_eq!(line_end().parse("\n"), Ok(((), "")));
    }

    #[test]
    fn line_end_eof() {
        assert_eq!(line_end().parse(""), Ok(((), "")));
    }

    #[test]
    fn typed_line_v() {
        assert_eq!(typed_line('v', token('0')).parse("v=0"), Ok(('0', "")))
    }

    #[test]
    fn attribute_line_flag_foo() {
        assert_eq!(attribute_line_flag("foo").parse("a=foo"), Ok(((), "")))
    }

    #[test]
    fn attribute_line_foo_bar() {
        assert_eq!(
            attribute_line("foo", any_value()).parse("a=foo:bar"),
            Ok(("bar".to_string(), ""))
        )
    }

    #[test]
    fn session_attribute_line_simple() {
        let x = session_attribute_line().parse("a=ice-lite");
        assert_eq!(x, Ok((SessionAttribute::IceLite, "")));
    }

    #[test]
    fn session_attribute_line_finger() {
        let x = session_attribute_line().parse("a=fingerprint:sha-256 45:AD:5C:82:F8:BE");
        assert_eq!(
            x,
            Ok((
                SessionAttribute::Fingerprint(Fingerprint {
                    hash_func: "sha-256".to_string(),
                    bytes: vec![69, 173, 92, 130, 248, 190],
                }),
                ""
            ))
        );
    }

    #[test]
    fn media_attribute_line_fmtp_horizontal_whitespace() {
        let expected = MediaAttribute::Fmtp {
            pt: 111.into(),
            values: vec![FormatParam::MinPTime(10), FormatParam::UseInbandFec(true)],
        };
        for params in [
            "minptime=10;\tuseinbandfec=1",
            "\tminptime=10;useinbandfec=1",
            "minptime=10\t;useinbandfec=1",
            "minptime=10;useinbandfec=1\t",
            " \tminptime=10 \t; \tuseinbandfec=1 \t; \t",
        ] {
            for end in ["\r\n", "\n", ""] {
                let next = if end.is_empty() { "" } else { "a=mid:0" };
                let line = format!("a=fmtp:111 {params}{end}{next}");
                assert_eq!(
                    media_attribute_line().parse(line.as_str()),
                    Ok((expected.clone(), next)),
                    "{line:?}"
                );
            }
        }
    }

    #[test]
    fn media_attribute_line_fmtp_preserves_next_line() {
        for end in ["\r\n", "\n"] {
            for next in ["a=mid:0", "a=sendrecv"] {
                let line = format!("a=fmtp:111 minptime=10;useinbandfec=1;{end}{next}");
                let expected = MediaAttribute::Fmtp {
                    pt: 111.into(),
                    values: vec![FormatParam::MinPTime(10), FormatParam::UseInbandFec(true)],
                };
                assert_eq!(
                    media_attribute_line().parse(line.as_str()),
                    Ok((expected, next))
                );
            }
        }
    }

    #[test]
    fn media_attribute_line_fmtp_separator_whitespace() {
        let expected = MediaAttribute::Fmtp {
            pt: 111.into(),
            values: vec![FormatParam::MinPTime(10), FormatParam::UseInbandFec(true)],
        };
        for line in [
            "a=fmtp:111 minptime=10;useinbandfec=1;\r\na=rtcp-fb:111 nack",
            "a=fmtp:111 minptime=10;useinbandfec=1 \r\na=rtcp-fb:111 nack",
            "a=fmtp:111 minptime=10 ;useinbandfec=1\r\na=rtcp-fb:111 nack",
            "a=fmtp:111 minptime=10; useinbandfec=1; \r\na=rtcp-fb:111 nack",
        ] {
            assert_eq!(
                media_attribute_line().parse(line),
                Ok((expected.clone(), "a=rtcp-fb:111 nack")),
                "{line:?}"
            );
        }
    }

    #[test]
    fn media_attribute_line_rid_simple() {
        let x = media_attribute_line().parse("a=rid:lo send").unwrap();
        assert_eq!("a=rid:lo send\r\n", x.0.to_string());
    }

    #[test]
    fn media_attribute_line_rid_pt() {
        let x = media_attribute_line()
            .parse("a=rid:lo send pt=99,100")
            .unwrap();
        assert_eq!("a=rid:lo send pt=99,100\r\n", x.0.to_string());
    }

    #[test]
    fn media_attribute_line_rid_restr() {
        let x = media_attribute_line()
            .parse("a=rid:lo send max-br=64000;max-height=360")
            .unwrap();
        assert_eq!(
            "a=rid:lo send max-br=64000;max-height=360\r\n",
            x.0.to_string()
        );
    }

    #[test]
    fn media_attribute_line_rid_pt_restr() {
        let x = media_attribute_line()
            .parse("a=rid:lo send pt=99,100;max-br=64000;max-height=360")
            .unwrap();
        assert_eq!(
            "a=rid:lo send pt=99,100;max-br=64000;max-height=360\r\n",
            x.0.to_string()
        );
    }

    #[test]
    fn media_attribute_line_simulcast() {
        let x = media_attribute_line()
            .parse("a=simulcast:send 3;4")
            .unwrap();
        assert_eq!("a=simulcast:send 3;4\r\n", x.0.to_string());
    }

    #[test]
    fn media_attribute_line_simulcast_alt() {
        let x = media_attribute_line()
            .parse("a=simulcast:send 2,3;4")
            .unwrap();
        assert_eq!("a=simulcast:send 2;4\r\n", x.0.to_string());
    }

    #[test]
    fn media_attribute_line_simulcast_send_recv() {
        let x = media_attribute_line()
            .parse("a=simulcast:send 2;3 recv 4")
            .unwrap();
        assert_eq!("a=simulcast:send 2;3 recv 4\r\n", x.0.to_string());
    }

    #[test]
    fn media_attribute_line_simulcast_recv_send() {
        let x = media_attribute_line()
            .parse("a=simulcast:recv 2;3 send 4")
            .unwrap();
        assert_eq!("a=simulcast:send 4 recv 2;3\r\n", x.0.to_string());
    }

    #[test]
    fn media_line_simple() {
        let m = media_line().parse("m=audio 9 UDP/TLS/RTP/SAVPF 10\r\n");
        assert_eq!(
            m,
            Ok((
                (MediaType::Audio, "9".into(), Proto::Srtp, vec![10.into()],),
                ""
            ))
        );
    }

    #[test]
    fn rtpmap_payload_type_over_127_is_rejected() {
        // RTP payload type is 7-bit. An out-of-range PT in a=rtpmap must not be accepted as an
        // RtpMap; otherwise it becomes a PayloadParams that panics when claimed against a
        // 128-entry table. It instead falls through to Unused and is ignored (the m-line then
        // fails its own "missing a=rtpmap" check). 127 is the largest valid PT.
        let bad = media_attribute_line().parse("a=rtpmap:200 opus/48000/2");
        assert!(
            !matches!(bad, Ok((MediaAttribute::RtpMap { .. }, _))),
            "out-of-range rtpmap PT must not parse as RtpMap: {bad:?}"
        );
        assert!(matches!(
            media_attribute_line().parse("a=rtpmap:127 opus/48000/2"),
            Ok((MediaAttribute::RtpMap { .. }, _))
        ));
    }

    #[test]
    fn session_parser_simple() {
        let sdp = "v=0\n\
            o=- 6564425948916445306 2 IN IP4 127.0.0.1\n\
            s=-\n\
            t=0 0\n\
            a=group:BUNDLE 0\n\
            a=msid-semantic: WMS\n\
            m=application 9 DTLS/SCTP 5000\n\
            ";
        assert_eq!(
            session_parser().parse(sdp),
            Ok((
                Session {
                    id: 6_564_425_948_916_445_306.into(),
                    bw: None,
                    attrs: vec![
                        SessionAttribute::Group {
                            typ: "BUNDLE".into(),
                            mids: vec!["0".into()],
                        },
                        SessionAttribute::Unused("msid-semantic: WMS".into())
                    ],
                },
                "m=application 9 DTLS/SCTP 5000\n"
            ))
        );
    }

    #[test]
    fn session_parser_group_without_mids() {
        for group in ["a=group:BUNDLE\r\n", "a=group:BUNDLE \r\n"] {
            let sdp = format!(
                "v=0\r\n\
                o=- 6564425948916445306 2 IN IP4 127.0.0.1\r\n\
                s=-\r\n\
                t=0 0\r\n\
                {group}"
            );
            let (session, rest) = session_parser().parse(sdp.as_str()).unwrap();
            assert_eq!(
                session.attrs,
                vec![SessionAttribute::Group {
                    typ: "BUNDLE".into(),
                    mids: vec![],
                }]
            );
            assert_eq!(rest, "");
        }
    }

    #[test]
    fn session_parser_named_session() {
        // A session name other than "-".
        let sdp = "v=0\r\n\
            o=publisher 4489045141692799359 2 IN IP4 127.0.0.1\r\n\
            s=NamedPublishSession\r\n\
            t=0 0\r\n\
            a=group:BUNDLE 0 1\r\n\
            a=extmap-allow-mixed\r\n\
            a=msid-semantic: WMS\r\n";
        assert_eq!(
            session_parser().parse(sdp),
            Ok((
                Session {
                    id: 4_489_045_141_692_799_359.into(),
                    bw: None,
                    attrs: vec![
                        SessionAttribute::Group {
                            typ: "BUNDLE".into(),
                            mids: vec!["0".into(), "1".into()],
                        },
                        SessionAttribute::Unused("extmap-allow-mixed".into()),
                        SessionAttribute::Unused("msid-semantic: WMS".into()),
                    ],
                },
                ""
            ))
        );
    }

    #[test]
    fn session_parser_space_session_name() {
        // "If a session has no meaningful name, the value "s= " SHOULD be used
        // (i.e., a single space as the session name)." (RFC 8866 section 5.3)
        let sdp = "v=0\n\
            o=- 6564425948916445306 2 IN IP4 127.0.0.1\n\
            s= \n\
            t=0 0\n\
            ";
        assert_eq!(
            session_parser().parse(sdp),
            Ok((
                Session {
                    id: 6_564_425_948_916_445_306.into(),
                    bw: None,
                    attrs: vec![],
                },
                ""
            ))
        );
    }

    #[test]
    fn parse_sdp_firefox() {
        let sdp = "v=0\r\n\
            o=mozilla...THIS_IS_SDPARTA-83.0 7052848360639826063 0 IN IP4 0.0.0.0\r\n\
            s=-\r\n\
            t=0 0\r\n\
            a=fingerprint:sha-256 \
                37:FC:96:B5:73:98:E6:F9:C5:0B:D9:EE:B1:F8:D0:01:07:2E:75:E8:6C:A4:32:A7:DC:63:\
                99:5E:68:5C:BF:FB\r\n\
            a=ice-options:trickle\r\n\
            a=msid-semantic:WMS *\r\n";

        assert_eq!(
            sdp_parser().parse(sdp),
            Ok((
                Sdp {
                    session: Session {
                        id: 7052848360639826063.into(),
                        bw: None,
                        attrs: vec![
                            SessionAttribute::Fingerprint(Fingerprint {
                                hash_func: "sha-256".into(),
                                bytes: vec![
                                    0x37, 0xFC, 0x96, 0xB5, 0x73, 0x98, 0xE6, 0xF9, 0xC5, 0x0B,
                                    0xD9, 0xEE, 0xB1, 0xF8, 0xD0, 0x01, 0x07, 0x2E, 0x75, 0xE8,
                                    0x6C, 0xA4, 0x32, 0xA7, 0xDC, 0x63, 0x99, 0x5E, 0x68, 0x5C,
                                    0xBF, 0xFB
                                ]
                            }),
                            SessionAttribute::IceOptions("trickle".to_string()),
                            SessionAttribute::MsidSemantic {
                                semantic: "WMS".into(),
                                stream_ids: vec!["*".into()]
                            },
                        ]
                    },
                    media_lines: vec![]
                },
                ""
            ))
        );
    }

    #[test]
    fn parse_offer_sdp_firefox() {
        let sdp = "v=0\r\n\
            o=mozilla...THIS_IS_SDPARTA-84.0 9033133899747520364 1 IN IP4 0.0.0.0\r\n\
            s=-\r\n\
            t=0 0\r\n\
            a=fingerprint:sha-256 \
                AE:DC:49:AE:CA:55:35:CB:4E:FA:FE:70:99:30:C0:14:C3:B8:06:80:1F:A9:DA:9A:7C:FB:\
                B7:20:AB:83:60:45\r\n\
            a=group:BUNDLE 0\r\n\
            a=ice-options:trickle\r\n\
            a=msid-semantic:WMS *\r\n\
            m=audio 9 UDP/TLS/RTP/SAVPF 109 9 0 8 101\r\n\
            c=IN IP4 0.0.0.0\r\n\
            a=sendonly\r\n\
            a=extmap:1 urn:ietf:params:rtp-hdrext:ssrc-audio-level\r\n\
            a=extmap:2/recvonly urn:ietf:params:rtp-hdrext:csrc-audio-level\r\n\
            a=extmap:3 urn:ietf:params:rtp-hdrext:sdes:mid\r\n\
            a=fmtp:109 maxplaybackrate=48000;stereo=1;useinbandfec=1\r\n\
            a=fmtp:101 0-15\r\n\
            a=ice-pwd:cd25258044061ec2ecc73378eb3dc6a3\r\n\
            a=ice-ufrag:c1e284ad\r\n\
            a=mid:0\r\n\
            a=msid:- {5c7f12e5-b4bd-7142-9a06-2885a2d1cb66}\r\n\
            a=rtcp-mux\r\n\
            a=rtpmap:109 opus/48000/2\r\n\
            a=rtpmap:9 G722/8000/1\r\n\
            a=rtpmap:0 PCMU/8000\r\n\
            a=rtpmap:8 PCMA/8000\r\n\
            a=rtpmap:101 telephone-event/8000/1\r\n\
            a=setup:actpass\r\n\
            a=ssrc:1481683531 cname:{326ec0d2-d1ae-974c-b1ad-aea85cdfa0ad}\r\n";

        let parsed = sdp_parser().parse(sdp);

        assert!(parsed.is_ok());
    }

    #[test]
    fn parse_offer_sdp_chrome() {
        let sdp = "v=0\r\n\
            o=- 5058682828002148772 3 IN IP4 127.0.0.1\r\n\
            s=-\r\n\
            t=0 0\r\n\
            a=group:BUNDLE 0\r\n\
            a=msid-semantic: WMS 5UUdwiuY7OML2EkQtF38pJtNP5v7In1LhjEK\r\n\
            m=audio 9 UDP/TLS/RTP/SAVPF 111 103 104 9 0 8 106 105 13 110 112 113 126\r\n\
            c=IN IP4 0.0.0.0\r\n\
            a=rtcp:9 IN IP4 0.0.0.0\r\n\
            a=ice-ufrag:S5hk\r\n\
            a=ice-pwd:0zV/Yu3y8aDzbHgqWhnVQhqP\r\n\
            a=ice-options:trickle\r\n\
            a=fingerprint:sha-256 \
                8C:64:ED:03:76:D0:3D:B4:88:08:91:64:08:80:A8:C6:5A:BF:8B:4E:38:27:96:CA:08:49:\
                25:73:46:60:20:DC\r\n\
            a=setup:actpass\r\n\
            a=mid:0\r\n\
            a=extmap:1 urn:ietf:params:rtp-hdrext:ssrc-audio-level\r\n\
            a=extmap:2 http://www.webrtc.org/experiments/rtp-hdrext/abs-send-time\r\n\
            a=extmap:3 http://www.ietf.org/id/draft-holmer-rmcat-transport-wide-cc-extensions-01\r\n\
            a=extmap:4 urn:ietf:params:rtp-hdrext:sdes:mid\r\n\
            a=extmap:5 urn:ietf:params:rtp-hdrext:sdes:rtp-stream-id\r\n\
            a=extmap:6 urn:ietf:params:rtp-hdrext:sdes:repaired-rtp-stream-id\r\n\
            a=sendrecv\r\n\
            a=msid:5UUdwiuY7OML2EkQtF38pJtNP5v7In1LhjEK f78dde68-7055-4e20-bb37-433803dd1ed1\r\n\
            a=rtcp-mux\r\n\
            a=rtpmap:111 opus/48000/2\r\n\
            a=rtcp-fb:111 transport-cc\r\n\
            a=fmtp:111 minptime=10;useinbandfec=1\r\n\
            a=rtpmap:103 ISAC/16000\r\n\
            a=rtpmap:104 ISAC/32000\r\n\
            a=rtpmap:9 G722/8000\r\n\
            a=rtpmap:0 PCMU/8000\r\n\
            a=rtpmap:8 PCMA/8000\r\n\
            a=rtpmap:106 CN/32000\r\n\
            a=rtpmap:105 CN/16000\r\n\
            a=rtpmap:13 CN/8000\r\n\
            a=rtpmap:110 telephone-event/48000\r\n\
            a=rtpmap:112 telephone-event/32000\r\n\
            a=rtpmap:113 telephone-event/16000\r\n\
            a=rtpmap:126 telephone-event/8000\r\n\
            a=ssrc:3948621874 cname:xeXs3aE9AOBn00yJ\r\n\
            a=ssrc:3948621874 msid:5UUdwiuY7OML2EkQtF38pJtNP5v7In1LhjEK \
                f78dde68-7055-4e20-bb37-433803dd1ed1\r\n\
            a=ssrc:3948621874 mslabel:5UUdwiuY7OML2EkQtF38pJtNP5v7In1LhjEK\r\n\
            a=ssrc:3948621874 label:f78dde68-7055-4e20-bb37-433803dd1ed1\r\n\
            ";

        let parsed = sdp_parser().parse(sdp);

        assert!(parsed.is_ok());
    }

    #[test]
    fn parse_offer_sdp_ffmpeg_whip() {
        // ffmpeg -re \
        //   -f lavfi -i "testsrc=size=640x480:rate=30" \
        //   -f lavfi -i "sine=frequency=440:sample_rate=48000" \
        //   -c:v libx264 -profile:v baseline -pix_fmt yuv420p -g 60 -tune zerolatency \
        //   -c:a libopus -ar 48000 -ac 2 \
        //   -f whip "http://127.0.0.1:8080/whip"
        let sdp = "v=0\r\n\
            o=FFmpeg 4489045141692799359 2 IN IP4 127.0.0.1\r\n\
            s=FFmpegPublishSession\r\n\
            t=0 0\r\n\
            a=group:BUNDLE 0 1\r\n\
            a=extmap-allow-mixed\r\n\
            a=msid-semantic: WMS\r\n\
            m=audio 9 UDP/TLS/RTP/SAVPF 111\r\n\
            c=IN IP4 0.0.0.0\r\n\
            a=ice-ufrag:08a9c3f4\r\n\
            a=ice-pwd:a202d23be47d1decaa6f67d04d4e6c16\r\n\
            a=fingerprint:sha-256 21:69:7C:46:D3:DC:4D:D3:66:61:5C:C5:6A:0B:F7:36\
            :BD:91:A9:2A:FF:BC:1A:59:D9:38:38:C8:E2:A4:8D:AF\r\n\
            a=setup:passive\r\n\
            a=mid:0\r\n\
            a=sendonly\r\n\
            a=msid:FFmpeg audio\r\n\
            a=rtcp-mux\r\n\
            a=rtpmap:111 opus/48000/2\r\n\
            a=ssrc:3166090378 cname:FFmpeg\r\n\
            a=ssrc:3166090378 msid:FFmpeg audio\r\n\
            m=video 9 UDP/TLS/RTP/SAVPF 106 105\r\n\
            c=IN IP4 0.0.0.0\r\n\
            a=ice-ufrag:08a9c3f4\r\n\
            a=ice-pwd:a202d23be47d1decaa6f67d04d4e6c16\r\n\
            a=fingerprint:sha-256 21:69:7C:46:D3:DC:4D:D3:66:61:5C:C5:6A:0B:F7:36\
            :BD:91:A9:2A:FF:BC:1A:59:D9:38:38:C8:E2:A4:8D:AF\r\n\
            a=setup:passive\r\n\
            a=mid:1\r\n\
            a=sendonly\r\n\
            a=msid:FFmpeg video\r\n\
            a=rtcp-mux\r\n\
            a=rtcp-rsize\r\n\
            a=rtpmap:106 H264/90000\r\n\
            a=fmtp:106 level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42001e\r\n\
            a=rtcp-fb:106 nack\r\n\
            a=rtpmap:105 rtx/90000\r\n\
            a=fmtp:105 apt=106\r\n\
            a=ssrc-group:FID 3166090379 3166090380\r\n\
            a=ssrc:3166090379 cname:FFmpeg\r\n\
            a=ssrc:3166090379 msid:FFmpeg video\r\n\
            ";

        let (sdp, _) = sdp_parser().parse(sdp).unwrap();

        assert_eq!(sdp.media_lines.len(), 2);
        assert_eq!(sdp.media_lines[0].mid().to_string(), "0");
        assert_eq!(sdp.media_lines[1].mid().to_string(), "1");
    }

    #[test]
    fn parse_safari_data_channel() {
        let sdp = "v=0\r\n\
        o=- 4611516372927609806 2 IN IP4 127.0.0.1\r\n\
        s=-\r\n\
        t=0 0\r\n\
        a=group:BUNDLE 0\r\n\
        a=extmap-allow-mixed\r\n\
        a=msid-semantic: WMS\r\n\
        m=application 9 UDP/DTLS/SCTP webrtc-datachannel\r\n\
        c=IN IP4 0.0.0.0\r\n\
        a=ice-ufrag:HhS+\r\n\
        a=ice-pwd:FhYTGhlAtKCe6KFIX8b+AThW\r\n\
        a=ice-options:trickle\r\n\
        a=fingerprint:sha-256 \
            B4:12:1C:7C:7D:ED:F1:FA:61:07:57:9C:29:BE:58:E3:BC:41:E7:13:8E:7D:D3:9D:1F:94:6E:\
            A5:23:46:94:23\r\n\
        a=setup:actpass\r\n\
        a=mid:0\r\n\
        a=sctp-port:5000\r\n\
        a=max-message-size:262144\r\n\
        ";

        let (sdp, _) = sdp_parser().parse(sdp).unwrap();

        assert!(!sdp.media_lines.is_empty());
        assert_eq!(sdp.media_lines[0].mid().to_string(), "0");
    }

    #[test]
    fn parse_minimal() {
        let sdp = "v=0\r\n\
        o=- 5058682828002148772 3 IN IP4 127.0.0.1\r\n\
        s=-\r\n\
        t=0 0\r\n\
        m=audio 9 UDP/TLS/RTP/SAVPF\r\n\
        c=IN IP4 0.0.0.0\r\n\
        a=rtcp:9 IN IP4 0.0.0.0\r\n\
        a=setup:actpass\r\n\
        a=inactive\r\n\
        a=mid:0\r\n\
        ";

        let parsed = sdp_parser().parse(sdp);
        assert!(parsed.is_ok());
    }

    #[test]
    fn bare_static_audio_pts_are_resolved() {
        let sdp = "v=0\r\n\
        o=- 1 1 IN IP4 127.0.0.1\r\n\
        s=-\r\n\
        t=0 0\r\n\
        a=group:BUNDLE 0\r\n\
        m=audio 9 UDP/TLS/RTP/SAVPF 0 8 9 13\r\n\
        c=IN IP4 0.0.0.0\r\n\
        a=setup:actpass\r\n\
        a=sendrecv\r\n\
        a=mid:0\r\n\
        ";

        let (sdp, _) = sdp_parser()
            .parse(sdp)
            .expect("static PTs must not require an a=rtpmap line");
        sdp.assert_consistency()
            .expect("bare static PTs are consistent");
        let params = sdp.media_lines[0].rtp_params();

        let definitions: Vec<_> = params
            .iter()
            .map(|p| (*p.pt(), p.spec().codec, p.spec().clock_rate))
            .collect();
        assert_eq!(
            definitions,
            vec![
                (0, Codec::PCMU, Frequency::EIGHT_KHZ),
                (8, Codec::PCMA, Frequency::EIGHT_KHZ),
                (9, Codec::G722, Frequency::SIXTEEN_KHZ),
                (13, Codec::CN, Frequency::EIGHT_KHZ),
            ]
        );
    }

    #[test]
    fn parse_candidates() {
        let a = "a=candidate:1 1 udp 1845494015 198.51.100.100 11100 typ srflx raddr \
            203.0.113.100 rport 10100 ufrag abc\r\n";

        let (c, _) = candidate_attribute().parse(a).unwrap();
        assert_eq!(c.ufrag(), Some("abc"));

        let a = "a=candidate:1 1 udp 1845494015 198.51.100.100 11100 typ host ufrag abc\r\n";
        let (c, _) = candidate_attribute().parse(a).unwrap();
        assert_eq!(c.ufrag(), Some("abc"));

        let a = "a=candidate:3684617590 1 udp 2122260223 10.217.229.219 50028 typ host \
            generation 0 network-id 1 network-cost 900";
        let (c, _) = candidate_attribute().parse(a).unwrap();
        assert_eq!(c.addr(), "10.217.229.219:50028".parse().unwrap());

        let a = "a=candidate:387183333 1 udp 1686052607 113.185.55.72 31267 typ srflx raddr \
            10.217.229.219 rport 50028 generation 0 network-id 1 network-cost 900";
        let (c, _) = candidate_attribute().parse(a).unwrap();
        assert_eq!(c.addr(), "113.185.55.72:31267".parse().unwrap());

        let a = "a=candidate:2501718406 1 tcp 1518280447 10.217.229.219 9 typ host tcptype \
            active generation 0 network-id 1 network-cost 900";
        let (c, _) = candidate_attribute().parse(a).unwrap();
        assert_eq!(c.addr(), "10.217.229.219:9".parse().unwrap());

        let a = "a=candidate:387183333 1 udp 1686052607 113.185.55.72 41775 typ srflx raddr \
            10.217.229.219 rport 50028 generation 0 network-id 1 network-cost 900";
        let (c, _) = candidate_attribute().parse(a).unwrap();
        assert_eq!(c.addr(), "113.185.55.72:41775".parse().unwrap());

        let a = "a=candidate:3936339338 1 udp 2122265343 fd00:f8aa:3ff5:5914:90bf:3c5:3378:9d4b 59125 \
            typ host generation 0 network-id 4 network-cost 50";
        let (c, _) = candidate_attribute().parse(a).unwrap();
        assert_eq!(
            c.addr(),
            "[fd00:f8aa:3ff5:5914:90bf:3c5:3378:9d4b]:59125"
                .parse()
                .unwrap()
        );
        assert_eq!(c.proto(), Protocol::Udp);

        let a = "a=candidate:2113932030 1 tcp 2113932030 142.250.82.253 19305 \
            typ host tcptype passive generation 0";
        let (c, _) = candidate_attribute().parse(a).unwrap();
        assert_eq!(c.addr(), "142.250.82.253:19305".parse().unwrap());
        assert_eq!(c.proto(), Protocol::Tcp);
        assert_eq!(c.tcptype(), Some(TcpType::Passive));

        let a =
            "a=candidate:2113932029 1 ssltcp 2113932029 142.250.82.252 443 typ host generation 0";
        let (c, _) = candidate_attribute().parse(a).unwrap();
        assert_eq!(c.addr(), "142.250.82.252:443".parse().unwrap());
        assert_eq!(c.proto(), Protocol::SslTcp);
        assert_eq!(c.tcptype(), None);
    }

    #[test]
    fn parse_firefox_missing_setup_on_mid1() {
        let sdp = "v=0\r\n\
        o=mozilla...THIS_IS_SDPARTA-99.0 7710052215259647220 2 IN IP4 0.0.0.0\r\n\
        s=-\r\n\
        t=0 0\r\n\
        a=fingerprint:sha-256 \
            A6:64:23:37:94:7E:4B:40:F6:62:86:8C:DD:09:D5:08:7E:D4:0E:68:58:93:45:EC:99:F2:91:\
            F7:19:72:E7:BB\r\n\
        a=group:BUNDLE 0 hxI i1X mxk B3D kNI nbB xIZ bKm Hkn\r\n\
        a=ice-options:trickle\r\n\
        a=msid-semantic:WMS *\r\n\
        m=audio 0 UDP/TLS/RTP/SAVPF 0\r\n\
        c=IN IP4 0.0.0.0\r\n\
        a=inactive\r\n\
        a=mid:1\r\n\
        a=rtpmap:0 PCMU/8000\r\n\
        ";

        let (sdp, _) = sdp_parser().parse(sdp).unwrap();

        // Firefox 'a=setup' attribute can be missing
        // the "a=setup:" sdp attribute (which should be mandatory)
        // - https://www.rfc-editor.org/rfc/rfc5763#section-5
        // - https://www.rfc-editor.org/rfc/rfc4145

        assert!(sdp.media_lines[0].setup().is_none()); // must not crash
    }

    #[test]
    fn parse_no_media_c_line() {
        let sdp = "v=0\r\n\
        o=- 0 0 IN IP4 172.17.0.1\r\n\
        s=-\r\n\
        c=IN IP4 172.17.0.1\r\n\
        t=0 0\r\n\
        m=application 9999 UDP/DTLS/SCTP webrtc-datachannel\r\n\
        a=mid:0\r\n\
        a=ice-options:ice2\r\n\
        a=ice-ufrag:libp2p+webrtc+v1/a75469cf670c4079f8c06af4a963c8a1:\
            libp2p+webrtc+v1/a75469cf670c4079f8c06af4a963c8a1\r\n\
        a=ice-pwd:libp2p+webrtc+v1/a75469cf670c4079f8c06af4a963c8a1:\
            libp2p+webrtc+v1/a75469cf670c4079f8c06af4a963c8a1\r\n\
        a=fingerprint:sha-256 \
            FF:FF:FF:FF:FF:FF:FF:FF:FF:FF:FF:FF:FF:FF:FF:FF:FF:FF:FF:FF:FF:FF:FF:FF:FF:FF:FF:\
            FF:FF:FF:FF:FF\r\n\
        a=setup:actpass\r\n\
        a=sctp-port:5000\r\n\
        a=max-message-size:16384\r\n\
        ";

        let parsed = sdp_parser().parse(sdp);
        println!("{:?}", parsed);
        parsed.expect("to parse ok");
    }

    /// Test parsing an SDP offer containing H.265 with sprop-max-don-diff.
    /// Verifies the fmtp line is correctly parsed into FormatParam::SpropMaxDonDiff
    /// alongside H265ProfileTierLevel.
    #[test]
    fn parse_sdp_h265_with_sprop_max_don_diff() {
        // Fingerprint split across lines to stay within 110 cols.
        let sdp = concat!(
            "v=0\r\n",
            "o=- 1234567890 2 IN IP4 127.0.0.1\r\n",
            "s=-\r\n",
            "t=0 0\r\n",
            "a=group:BUNDLE 0\r\n",
            "a=fingerprint:sha-256 ",
            "AA:BB:CC:DD:EE:FF:00:11:22:33:44:55:",
            "66:77:88:99:AA:BB:CC:DD:EE:FF:00:11:",
            "22:33:44:55:66:77:88:99\r\n",
            "a=msid-semantic: WMS\r\n",
            "m=video 9 UDP/TLS/RTP/SAVPF 102 103\r\n",
            "c=IN IP4 0.0.0.0\r\n",
            "a=rtcp:9 IN IP4 0.0.0.0\r\n",
            "a=ice-ufrag:abcd\r\n",
            "a=ice-pwd:abcdefghijklmnopqrstuv\r\n",
            "a=ice-options:trickle\r\n",
            "a=fingerprint:sha-256 ",
            "AA:BB:CC:DD:EE:FF:00:11:22:33:44:55:",
            "66:77:88:99:AA:BB:CC:DD:EE:FF:00:11:",
            "22:33:44:55:66:77:88:99\r\n",
            "a=setup:actpass\r\n",
            "a=mid:0\r\n",
            "a=sendrecv\r\n",
            "a=rtpmap:102 H265/90000\r\n",
            "a=fmtp:102 profile-id=1;tier-flag=0;",
            "level-id=93;sprop-max-don-diff=32\r\n",
            "a=rtpmap:103 rtx/90000\r\n",
            "a=fmtp:103 apt=102\r\n",
        );

        let (sdp, _remaining) = sdp_parser().parse(sdp).expect("SDP should parse");

        // Find the video media section
        assert_eq!(sdp.media_lines.len(), 1);
        let media = &sdp.media_lines[0];

        // Find fmtp for PT 102 (H.265)
        let fmtp_values: Vec<_> = media
            .attrs
            .iter()
            .filter_map(|a| {
                if let MediaAttribute::Fmtp { pt, values } = a {
                    if *pt == 102.into() {
                        return Some(values.clone());
                    }
                }
                None
            })
            .flatten()
            .collect();

        // Should contain H265ProfileTierLevel
        assert!(
            fmtp_values
                .iter()
                .any(|p| matches!(p, FormatParam::H265ProfileTierLevel(_))),
            "Should have H265ProfileTierLevel param"
        );

        // Should contain SpropMaxDonDiff(32)
        assert!(
            fmtp_values
                .iter()
                .any(|p| matches!(p, FormatParam::SpropMaxDonDiff(32))),
            "Should have SpropMaxDonDiff(32) param, got: {:?}",
            fmtp_values
        );

        // Verify PTL values
        if let Some(FormatParam::H265ProfileTierLevel(ptl)) = fmtp_values
            .iter()
            .find(|p| matches!(p, FormatParam::H265ProfileTierLevel(_)))
        {
            assert_eq!(ptl.profile_id(), 1);
            assert_eq!(ptl.tier_flag(), 0);
            assert_eq!(ptl.level_id(), 93);
        } else {
            panic!("H265ProfileTierLevel not found");
        }
    }
}

// Safari addTransceiver('audio', {direction: 'sendonly'}))

// v=0
// o=- 1625652694357831865 2 IN IP4 127.0.0.1
// s=-
// t=0 0
// a=group:BUNDLE 0
// a=msid-semantic: WMS
// m=audio 9 UDP/TLS/RTP/SAVPF 111 103 9 102 0 8 105 13 110 113 126
// c=IN IP4 0.0.0.0\na=rtcp:9 IN IP4 0.0.0.0
// a=ice-ufrag:CTNF
// a=ice-pwd:XbIkl+k8mwLz60TFwb9crzbz
// a=ice-options:trickle
// a=fingerprint:sha-256 77:8C:32:B8:DA:4E:78:64:C8:4A:2A:E2:1D:60:2A:83:6B:51:B8:D5:EE:\
// 5A:ED:75:4E:C6:98:2D:78:4D:94:D8
// a=setup:actpass
// a=mid:0
// a=extmap:1 urn:ietf:params:rtp-hdrext:ssrc-audio-level
// a=extmap:9 urn:ietf:params:rtp-hdrext:sdes:mid
// a=sendonly
// a=msid:- 39a7c3c3-ab8c-4b25-a47b-db52d89c2db1
// a=rtcp-mux
// a=rtpmap:111 opus/48000/2
// a=rtcp-fb:111 transport-cc
// a=fmtp:111 minptime=10;useinbandfec=1
// a=rtpmap:103 ISAC/16000
// a=rtpmap:9 G722/8000
// a=rtpmap:102 ILBC/8000
// a=rtpmap:0 PCMU/8000
// a=rtpmap:8 PCMA/8000
// a=rtpmap:105 CN/16000
// a=rtpmap:13 CN/8000
// a=rtpmap:110 telephone-event/48000
// a=rtpmap:113 telephone-event/16000
// a=rtpmap:126 telephone-event/8000
// a=ssrc:457025658 cname:6OQ+jzZE+UnQgSUr
// a=ssrc:457025658 msid:- 39a7c3c3-ab8c-4b25-a47b-db52d89c2db1
// a=ssrc:457025658 mslabel:-
// a=ssrc:457025658 label:39a7c3c3-ab8c-4b25-a47b-db52d89c2db1

// Chrome addTransceiver('audio', {direction: 'sendonly'}))

// v=0
// o=- 6740661649996974832 2 IN IP4 127.0.0.1
// s=-
// t=0 0
// a=group:BUNDLE 0
// a=msid-semantic: WMS
// m=audio 9 UDP/TLS/RTP/SAVPF 111 103 104 9 0 8 106 105 13 110 112 113 126
// c=IN IP4 0.0.0.0
// a=rtcp:9 IN IP4 0.0.0.0
// a=ice-ufrag:Y28C
// a=ice-pwd:W6fHYINgGi9QF0BHdM/kchTW
// a=ice-options:trickle
// a=fingerprint:sha-256 E0:C0:2D:52:8D:FA:14:69:A0:A5:5D:63:E0:82:92:DB:37:38:D2:F3:12:\
// D0:1F:4E:E5:6F:1A:F5:C3:97:6B:32
// a=setup:actpass
// a=mid:0
// a=extmap:1 urn:ietf:params:rtp-hdrext:ssrc-audio-level
// a=extmap:2 http://www.webrtc.org/experiments/rtp-hdrext/abs-send-time
// a=extmap:3 http://www.ietf.org/id/draft-holmer-rmcat-transport-wide-cc-extensions-01
// a=extmap:4 urn:ietf:params:rtp-hdrext:sdes:mid
// a=extmap:5 urn:ietf:params:rtp-hdrext:sdes:rtp-stream-id
// a=extmap:6 urn:ietf:params:rtp-hdrext:sdes:repaired-rtp-stream-id
// a=sendonly
// a=msid:- 7a08dda6-518f-4027-b707-410a6d414176
// a=rtcp-mux
// a=rtpmap:111 opus/48000/2
// a=rtcp-fb:111 transport-cc
// a=fmtp:111 minptime=10;useinbandfec=1
// a=rtpmap:103 ISAC/16000
// a=rtpmap:104 ISAC/32000
// a=rtpmap:9 G722/8000
// a=rtpmap:0 PCMU/8000
// a=rtpmap:8 PCMA/8000
// a=rtpmap:106 CN/32000
// a=rtpmap:105 CN/16000
// a=rtpmap:13 CN/8000
// a=rtpmap:110 telephone-event/48000
// a=rtpmap:112 telephone-event/32000
// a=rtpmap:113 telephone-event/16000
// a=rtpmap:126 telephone-event/8000
// a=ssrc:2147603131 cname:TbS1Ajv9obq6/63I
// a=ssrc:2147603131 msid:- 7a08dda6-518f-4027-b707-410a6d414176
// a=ssrc:2147603131 mslabel:-
// a=ssrc:2147603131 label:7a08dda6-518f-4027-b707-410a6d414176

// Chrome add video
// pc.addTransceiver("video", {
//     direction: 'sendonly',
//     sendEncodings: [
//         { rid: 'hi' },
//         { rid: 'lo', scaleResolutionDownBy: 2 },
//     ]
// })

// v=0
// o=- 2847298852000198709 3 IN IP4 127.0.0.1
// s=-
// t=0 0
// a=group:BUNDLE 0
// a=extmap-allow-mixed
// a=msid-semantic: WMS
// m=video 9 UDP/TLS/RTP/SAVPF 96 97 102 103 104 105 106 107 108 109 127 125 39 40 45 46 98 99 100 101 112 113
// 114
// c=IN IP4 0.0.0.0
// a=rtcp:9 IN IP4 0.0.0.0
// a=ice-ufrag:numD
// a=ice-pwd:L/hHurqERpVUACiI8To+vXLv
// a=ice-options:trickle
// a=fingerprint:sha-256 A5:79:72:59:3D:32:74:9C:44:70:DE:39:15:C3:99:51:32:6E:0D:F0:60:\
// DD:2F:31:90:E5:96:B4:1D:CA:48:E1
// a=setup:actpass
// a=mid:0
// a=extmap:1 urn:ietf:params:rtp-hdrext:toffset
// a=extmap:2 http://www.webrtc.org/experiments/rtp-hdrext/abs-send-time
// a=extmap:3 urn:3gpp:video-orientation
// a=extmap:4 http://www.ietf.org/id/draft-holmer-rmcat-transport-wide-cc-extensions-01
// a=extmap:5 http://www.webrtc.org/experiments/rtp-hdrext/playout-delay
// a=extmap:6 http://www.webrtc.org/experiments/rtp-hdrext/video-content-type
// a=extmap:7 http://www.webrtc.org/experiments/rtp-hdrext/video-timing
// a=extmap:8 http://www.webrtc.org/experiments/rtp-hdrext/color-space
// a=extmap:9 urn:ietf:params:rtp-hdrext:sdes:mid
// a=extmap:10 urn:ietf:params:rtp-hdrext:sdes:rtp-stream-id
// a=extmap:11 urn:ietf:params:rtp-hdrext:sdes:repaired-rtp-stream-id
// a=extmap:13 https://aomediacodec.github.io/av1-rtp-spec/#dependency-descriptor-rtp-header-extension
// a=extmap:14 http://www.webrtc.org/experiments/rtp-hdrext/video-layers-allocation00
// a=sendonly
// a=msid:- 7d27ad91-4770-4a4d-a053-2b06233753d2
// a=rtcp-mux
// a=rtcp-rsize
// a=rtpmap:96 VP8/90000
// a=rtcp-fb:96 goog-remb
// a=rtcp-fb:96 transport-cc
// a=rtcp-fb:96 ccm fir
// a=rtcp-fb:96 nack
// a=rtcp-fb:96 nack pli
// a=rtpmap:97 rtx/90000
// a=fmtp:97 apt=96
// a=rtpmap:102 H264/90000
// a=rtcp-fb:102 goog-remb
// a=rtcp-fb:102 transport-cc
// a=rtcp-fb:102 ccm fir
// a=rtcp-fb:102 nack
// a=rtcp-fb:102 nack pli
// a=fmtp:102 level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42001f
// a=rtpmap:103 rtx/90000
// a=fmtp:103 apt=102
// a=rtpmap:104 H264/90000
// a=rtcp-fb:104 goog-remb
// a=rtcp-fb:104 transport-cc
// a=rtcp-fb:104 ccm fir
// a=rtcp-fb:104 nack
// a=rtcp-fb:104 nack pli
// a=fmtp:104 level-asymmetry-allowed=1;packetization-mode=0;profile-level-id=42001f
// a=rtpmap:105 rtx/90000
// a=fmtp:105 apt=104
// a=rtpmap:106 H264/90000
// a=rtcp-fb:106 goog-remb
// a=rtcp-fb:106 transport-cc
// a=rtcp-fb:106 ccm fir
// a=rtcp-fb:106 nack
// a=rtcp-fb:106 nack pli
// a=fmtp:106 level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e01f
// a=rtpmap:107 rtx/90000
// a=fmtp:107 apt=106
// a=rtpmap:108 H264/90000
// a=rtcp-fb:108 goog-remb
// a=rtcp-fb:108 transport-cc
// a=rtcp-fb:108 ccm fir
// a=rtcp-fb:108 nack
// a=rtcp-fb:108 nack pli
// a=fmtp:108 level-asymmetry-allowed=1;packetization-mode=0;profile-level-id=42e01f
// a=rtpmap:109 rtx/90000
// a=fmtp:109 apt=108
// a=rtpmap:127 H264/90000
// a=rtcp-fb:127 goog-remb
// a=rtcp-fb:127 transport-cc
// a=rtcp-fb:127 ccm fir
// a=rtcp-fb:127 nack
// a=rtcp-fb:127 nack pli
// a=fmtp:127 level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=4d001f
// a=rtpmap:125 rtx/90000
// a=fmtp:125 apt=127
// a=rtpmap:39 H264/90000
// a=rtcp-fb:39 goog-remb
// a=rtcp-fb:39 transport-cc
// a=rtcp-fb:39 ccm fir
// a=rtcp-fb:39 nack
// a=rtcp-fb:39 nack pli
// a=fmtp:39 level-asymmetry-allowed=1;packetization-mode=0;profile-level-id=4d001f
// a=rtpmap:40 rtx/90000
// a=fmtp:40 apt=39
// a=rtpmap:45 AV1/90000
// a=rtcp-fb:45 goog-remb
// a=rtcp-fb:45 transport-cc
// a=rtcp-fb:45 ccm fir
// a=rtcp-fb:45 nack
// a=rtcp-fb:45 nack pli
// a=fmtp:45 level-idx=5;profile=0;tier=0
// a=rtpmap:46 rtx/90000
// a=fmtp:46 apt=45
// a=rtpmap:98 VP9/90000
// a=rtcp-fb:98 goog-remb
// a=rtcp-fb:98 transport-cc
// a=rtcp-fb:98 ccm fir
// a=rtcp-fb:98 nack
// a=rtcp-fb:98 nack pli
// a=fmtp:98 profile-id=0
// a=rtpmap:99 rtx/90000
// a=fmtp:99 apt=98
// a=rtpmap:100 VP9/90000
// a=rtcp-fb:100 goog-remb
// a=rtcp-fb:100 transport-cc
// a=rtcp-fb:100 ccm fir
// a=rtcp-fb:100 nack
// a=rtcp-fb:100 nack pli
// a=fmtp:100 profile-id=2
// a=rtpmap:101 rtx/90000
// a=fmtp:101 apt=100
// a=rtpmap:112 red/90000
// a=rtpmap:113 rtx/90000
// a=fmtp:113 apt=112
// a=rtpmap:114 ulpfec/90000
// a=rid:hi send
// a=rid:lo send
// a=simulcast:send hi;lo

// DataChannel only in Safari
// v=0
// o=- 4611516372927609806 2 IN IP4 127.0.0.1
// s=-
// t=0 0
// a=group:BUNDLE 0
// a=extmap-allow-mixed
// a=msid-semantic: WMS
// m=application 9 UDP/DTLS/SCTP webrtc-datachannel
// c=IN IP4 0.0.0.0
// a=ice-ufrag:HhS+
// a=ice-pwd:FhYTGhlAtKCe6KFIX8b+AThW
// a=ice-options:trickle
// a=fingerprint:sha-256 B4:12:1C:7C:7D:ED:F1:FA:61:07:57:9C:29:BE:58:E3:BC:41:E7:13:8E:\
// 7D:D3:9D:1F:94:6E:A5:23:46:94:23
// a=setup:actpass
// a=mid:0
// a=sctp-port:5000
// a=max-message-size:262144
