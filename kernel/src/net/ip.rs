//! IPv4 header parsing and building.

pub const IP_HDR_LEN: usize = 20;

pub mod proto {
    pub const ICMP: u8 = 1;
    pub const TCP:  u8 = 6;
    pub const UDP:  u8 = 17;
}

/// Parse a minimal IPv4 header (no options).
///
/// Returns `(src_ip, dst_ip, protocol, payload)` or `None` on error.
pub fn parse(pkt: &[u8]) -> Option<(u32, u32, u8, &[u8])> {
    if pkt.len() < IP_HDR_LEN { return None; }
    let ihl = (pkt[0] & 0xF) as usize * 4;
    if ihl < IP_HDR_LEN || pkt.len() < ihl { return None; }
    let total_len = u16::from_be_bytes([pkt[2], pkt[3]]) as usize;
    if pkt.len() < total_len { return None; }
    let proto = pkt[9];
    let src = u32::from_be_bytes(pkt[12..16].try_into().unwrap());
    let dst = u32::from_be_bytes(pkt[16..20].try_into().unwrap());
    Some((src, dst, proto, &pkt[ihl..total_len]))
}

/// Build a minimal IPv4 header (no options) into `buf[..20]`.
///
/// `payload_len` does not include the IP header.
pub fn build(buf: &mut [u8; IP_HDR_LEN], proto: u8,
             src: u32, dst: u32, payload_len: u16, id: u16) {
    buf[0]  = 0x45; // version=4, IHL=5
    buf[1]  = 0;    // DSCP / ECN
    let total = payload_len + 20;
    buf[2..4].copy_from_slice(&total.to_be_bytes());
    buf[4..6].copy_from_slice(&id.to_be_bytes());
    buf[6..8].copy_from_slice(&0u16.to_be_bytes()); // flags + fragment offset
    buf[8]  = 64;   // TTL
    buf[9]  = proto;
    buf[10..12].copy_from_slice(&0u16.to_be_bytes()); // checksum placeholder
    buf[12..16].copy_from_slice(&src.to_be_bytes());
    buf[16..20].copy_from_slice(&dst.to_be_bytes());

    // Compute checksum over the header.
    let csum = checksum(&*buf);
    buf[10..12].copy_from_slice(&csum.to_be_bytes());
}

/// Internet checksum (RFC 1071).
pub fn checksum(data: &[u8]) -> u16 {
    let mut sum: u32 = 0;
    let mut i = 0;
    while i + 1 < data.len() {
        sum += u16::from_be_bytes([data[i], data[i + 1]]) as u32;
        i += 2;
    }
    if i < data.len() {
        sum += (data[i] as u32) << 8;
    }
    while sum >> 16 != 0 {
        sum = (sum & 0xFFFF) + (sum >> 16);
    }
    !(sum as u16)
}
