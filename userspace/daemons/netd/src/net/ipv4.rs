//! IPv4 (RFC 791) header parse/emit with the mandatory header checksum. No
//! options, no fragmentation (DF set on emit; fragmented inbound packets are
//! dropped). netd never forwards, so inbound TTL is not decremented.

use super::wire::{be16, checksum, wr16};

pub const PROTO_ICMP: u8 = 1;

/// IPv4 header length without options.
pub const HDR_LEN: usize = 20;
/// Default TTL for emitted packets.
const DEFAULT_TTL: u8 = 64;
/// Don't-Fragment flag (bit 14 of the flags/fragment field).
const FLAG_DF: u16 = 0x4000;

/// A parsed IPv4 header plus its L4 payload (trimmed to `total_length`).
pub struct Ipv4Packet<'a> {
    pub src: [u8; 4],
    pub dst: [u8; 4],
    pub protocol: u8,
    pub payload: &'a [u8],
}

/// Parse and validate an IPv4 packet: version, IHL, header checksum, and a
/// `total_length` that fits the buffer. Returns `None` (drop) on any failure,
/// including fragmented packets.
pub fn parse(buf: &[u8]) -> Option<Ipv4Packet<'_>> {
    if buf.len() < HDR_LEN {
        return None;
    }
    let vihl = buf[0];
    if vihl >> 4 != 4 {
        return None; // not IPv4
    }
    let ihl = (vihl & 0x0F) as usize * 4;
    if ihl < HDR_LEN || buf.len() < ihl {
        return None;
    }
    if checksum(&buf[..ihl]) != 0 {
        return None; // corrupt header
    }
    // Drop fragments (MF set or non-zero fragment offset) — no reassembly.
    let flags_frag = be16(&buf[6..8]);
    if flags_frag & 0x2000 != 0 || flags_frag & 0x1FFF != 0 {
        return None;
    }
    let total_len = be16(&buf[2..4]) as usize;
    if total_len < ihl || total_len > buf.len() {
        return None;
    }
    let mut src = [0u8; 4];
    let mut dst = [0u8; 4];
    src.copy_from_slice(&buf[12..16]);
    dst.copy_from_slice(&buf[16..20]);
    Some(Ipv4Packet {
        src,
        dst,
        protocol: buf[9],
        payload: &buf[ihl..total_len],
    })
}

/// Write a 20-byte IPv4 header (no options) into `buf`, carrying `payload_len`
/// bytes of L4 data, and fill in the header checksum. `buf` must be ≥ 20.
pub fn write_header(
    buf: &mut [u8],
    src: &[u8; 4],
    dst: &[u8; 4],
    protocol: u8,
    payload_len: usize,
    ident: u16,
) {
    buf[0] = 0x45; // version 4, IHL 5
    buf[1] = 0; // DSCP/ECN
    wr16(&mut buf[2..4], (HDR_LEN + payload_len) as u16);
    wr16(&mut buf[4..6], ident);
    wr16(&mut buf[6..8], FLAG_DF);
    buf[8] = DEFAULT_TTL;
    buf[9] = protocol;
    wr16(&mut buf[10..12], 0); // checksum placeholder
    buf[12..16].copy_from_slice(src);
    buf[16..20].copy_from_slice(dst);
    let ck = checksum(&buf[..HDR_LEN]);
    wr16(&mut buf[10..12], ck);
}
