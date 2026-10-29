//! Compact 8-character invite codes (digits + letters, no ambiguous 0/1/I/O).
//!
//! For common private IPv4 + port we encode the address directly so the code
//! works offline with no directory server. Otherwise a random code is used and
//! resolved via Presence / discovery on the mesh.

use anyhow::{anyhow, Result};
use rand::Rng;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

/// Alphabet without 0, 1, I, O (easy to mistype).
const ALPHABET: &[u8] = b"23456789ABCDEFGHJKLMNPQRSTUVWXYZ";

pub fn normalize_code(raw: &str) -> String {
    raw.chars()
        .filter(|c| !c.is_whitespace() && *c != '-')
        .map(|c| match c.to_ascii_uppercase() {
            '0' | 'O' => '2',
            '1' | 'I' | 'L' => 'J',
            other => other,
        })
        .take(8)
        .collect()
}

pub fn looks_like_code(raw: &str) -> bool {
    let n = normalize_code(raw);
    n.len() == 8 && n.bytes().all(|c| ALPHABET.contains(&c))
}

/// Encode a reachable mesh address into an 8-char code when possible.
pub fn encode_addr(addr: SocketAddr) -> Option<String> {
    let SocketAddr::V4(v4) = addr else {
        return None;
    };
    let o = v4.ip().octets();
    let port = v4.port();
    // 40-bit payload → 8 × 5-bit symbols.
    // kind in top 2 bits:
    //   0 = 192.168.A.B:port
    //   1 = 10.A.B.C with port = 7400 + delta (0..63)
    //   2 = 100.A.B.C (Tailscale) same port packing
    let payload: u64 = match (o[0], o[1]) {
        (192, 168) => {
            let body = ((o[2] as u64) << 24) | ((o[3] as u64) << 16) | (port as u64);
            let chk = checksum6(body);
            (body << 6) | chk
        }
        (10, _) => {
            let delta = port.checked_sub(7400).filter(|&d| d < 64)?;
            let body = ((o[1] as u64) << 22)
                | ((o[2] as u64) << 14)
                | ((o[3] as u64) << 6)
                | (delta as u64);
            let chk = checksum6(body);
            (1u64 << 38) | (body << 6) | chk
        }
        (100, _) => {
            let delta = port.checked_sub(7400).filter(|&d| d < 64)?;
            let body = ((o[1] as u64) << 22)
                | ((o[2] as u64) << 14)
                | ((o[3] as u64) << 6)
                | (delta as u64);
            let chk = checksum6(body);
            (2u64 << 38) | (body << 6) | chk
        }
        _ => return None,
    };
    Some(encode40(payload))
}

pub fn decode_addr(code: &str) -> Result<SocketAddr> {
    let n = normalize_code(code);
    if n.len() != 8 {
        return Err(anyhow!("invite code must be 8 characters"));
    }
    let payload = decode40(&n)?;
    let kind = (payload >> 38) & 0b11;
    let chk = payload & 0x3f;
    let (ip, port, body_for_chk) = match kind {
        0 => {
            let body = (payload >> 6) & 0xffff_ffff;
            let a = ((body >> 24) & 0xff) as u8;
            let b = ((body >> 16) & 0xff) as u8;
            let port = (body & 0xffff) as u16;
            (Ipv4Addr::new(192, 168, a, b), port, body)
        }
        1 => {
            let body = (payload >> 6) & 0x3fff_ffff;
            let a = ((body >> 22) & 0xff) as u8;
            let b = ((body >> 14) & 0xff) as u8;
            let c = ((body >> 6) & 0xff) as u8;
            let delta = (body & 0x3f) as u16;
            (Ipv4Addr::new(10, a, b, c), 7400 + delta, body)
        }
        2 => {
            let body = (payload >> 6) & 0x3fff_ffff;
            let a = ((body >> 22) & 0xff) as u8;
            let b = ((body >> 14) & 0xff) as u8;
            let c = ((body >> 6) & 0xff) as u8;
            let delta = (body & 0x3f) as u16;
            (Ipv4Addr::new(100, a, b, c), 7400 + delta, body)
        }
        _ => return Err(anyhow!("unknown invite code kind")),
    };
    if checksum6(body_for_chk) != chk {
        return Err(anyhow!("invalid invite code checksum"));
    }
    if port == 0 {
        return Err(anyhow!("invalid invite port"));
    }
    Ok(SocketAddr::new(IpAddr::V4(ip), port))
}

/// Random 8-char code for addresses we cannot encode (persisted by caller).
pub fn random_code() -> String {
    let mut rng = rand::thread_rng();
    (0..8)
        .map(|_| ALPHABET[rng.gen_range(0..ALPHABET.len())] as char)
        .collect()
}

fn checksum6(body: u64) -> u64 {
    let mut x = body ^ (body >> 7) ^ (body << 3);
    x ^= x >> 11;
    x & 0x3f
}

fn encode40(mut payload: u64) -> String {
    payload &= (1u64 << 40) - 1;
    let mut out = [0u8; 8];
    for i in (0..8).rev() {
        out[i] = ALPHABET[(payload & 31) as usize];
        payload >>= 5;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn decode40(code: &str) -> Result<u64> {
    let mut payload = 0u64;
    for c in code.chars() {
        let u = c as u8;
        let Some(idx) = ALPHABET.iter().position(|&a| a == u) else {
            return Err(anyhow!("invalid invite code character"));
        };
        payload = (payload << 5) | (idx as u64);
    }
    Ok(payload)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_192() {
        let addr: SocketAddr = "192.168.1.77:7421".parse().unwrap();
        let code = encode_addr(addr).unwrap();
        assert_eq!(code.len(), 8);
        assert_eq!(decode_addr(&code).unwrap(), addr);
    }

    #[test]
    fn roundtrip_tailscale() {
        let addr: SocketAddr = "100.64.1.2:7421".parse().unwrap();
        let code = encode_addr(addr).unwrap();
        assert_eq!(decode_addr(&code).unwrap(), addr);
    }
}
