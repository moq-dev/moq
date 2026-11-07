use std::net::{IpAddr, SocketAddr};
use std::str::FromStr;

use crate::{Candidate, CandidateKind, IceError};
use str0m_proto::TcpType;

/// Parse a candidate string into a [Candidate].
///
/// Expects the format defined in RFC 5245 section 15.1, starting with `candidate:`.
/// Does not parse an `a=` prefix or trailing newline.
pub fn parse_candidate(s: &str) -> Result<Candidate, IceError> {
    parse_candidate_inner(s).map_err(|e| IceError::BadCandidate(format!("{}: {}", s, e)))
}

fn parse_candidate_inner(s: &str) -> Result<Candidate, String> {
    let s = s
        .strip_prefix("candidate:")
        .ok_or("missing 'candidate:' prefix")?;

    let mut iter = s.split_whitespace();

    let foundation = iter.next().ok_or("missing foundation")?.to_string();
    let component_id: u16 = iter
        .next()
        .ok_or("missing component-id")?
        .parse()
        .map_err(|e| format!("bad component-id: {e}"))?;
    let proto_str = iter.next().ok_or("missing protocol")?;
    let proto = proto_str
        .try_into()
        .map_err(|_| format!("invalid protocol: {proto_str}"))?;
    let prio: u32 = iter
        .next()
        .ok_or("missing priority")?
        .parse()
        .map_err(|e| format!("bad priority: {e}"))?;
    let addr: IpAddr = iter
        .next()
        .ok_or("missing address")?
        .parse()
        .map_err(|e| format!("bad address: {e}"))?;
    let port: u16 = iter
        .next()
        .ok_or("missing port")?
        .parse()
        .map_err(|e| format!("bad port: {e}"))?;

    // expect "typ"
    match iter.next() {
        Some("typ") => {}
        other => return Err(format!("expected 'typ', got: {:?}", other)),
    }

    let kind_str = iter.next().ok_or("missing candidate type")?;
    let kind = match kind_str {
        "host" => CandidateKind::Host,
        "prflx" => CandidateKind::PeerReflexive,
        "srflx" => CandidateKind::ServerReflexive,
        "relay" => CandidateKind::Relayed,
        other => return Err(format!("unknown candidate type: {other}")),
    };

    // Parse optional key-value extensions
    let mut raddr = None;
    let mut rport = None;
    let mut tcptype = None;
    let mut ufrag = None;

    while let Some(key) = iter.next() {
        match key {
            "raddr" => {
                let v = iter.next().ok_or("missing raddr value")?;
                raddr = Some(v.parse::<IpAddr>().map_err(|e| format!("bad raddr: {e}"))?);
            }
            "rport" => {
                let v = iter.next().ok_or("missing rport value")?;
                rport = Some(v.parse::<u16>().map_err(|e| format!("bad rport: {e}"))?);
            }
            "tcptype" => {
                let v = iter.next().ok_or("missing tcptype value")?;
                tcptype = Some(TcpType::from_str(v).map_err(|e| format!("bad tcptype: {e}"))?);
            }
            "ufrag" => {
                let v = iter.next().ok_or("missing ufrag value")?;
                ufrag = Some(v.to_string());
            }
            // Ignore unknown extensions (generation, network-id, network-cost, etc.)
            _ => {
                // consume the value
                let _ = iter.next();
            }
        }
    }

    let raddr_sock = match (raddr, rport) {
        (Some(ip), Some(port)) => Some(SocketAddr::from((ip, port))),
        _ => None,
    };

    Ok(Candidate::from_parts(
        foundation,
        component_id,
        proto,
        prio,
        SocketAddr::from((addr, port)),
        kind,
        raddr_sock,
        tcptype,
        ufrag,
    ))
}
