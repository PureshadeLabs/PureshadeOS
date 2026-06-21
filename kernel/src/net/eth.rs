//! Ethernet II frame parsing and building.

/// EtherType values.
pub mod ethertype {
    pub const IPV4: u16 = 0x0800;
    pub const ARP:  u16 = 0x0806;
}

/// Minimum Ethernet frame payload size.
pub const ETH_HDR_LEN: usize = 14;

/// Broadcast MAC address.
pub const MAC_BROADCAST: [u8; 6] = [0xFF; 6];

/// Build an Ethernet II frame into `buf`.
///
/// Returns the total frame length (14 + payload).  `buf` must be at least
/// `14 + payload.len()` bytes long.
pub fn build(buf: &mut [u8], dst: &[u8; 6], src: &[u8; 6], ethertype: u16, payload: &[u8]) -> usize {
    buf[0..6].copy_from_slice(dst);
    buf[6..12].copy_from_slice(src);
    buf[12..14].copy_from_slice(&ethertype.to_be_bytes());
    buf[14..14 + payload.len()].copy_from_slice(payload);
    14 + payload.len()
}

/// Parse the destination MAC, source MAC, and EtherType from a raw frame.
///
/// Returns `None` if `frame` is shorter than 14 bytes.
pub fn parse(frame: &[u8]) -> Option<([u8; 6], [u8; 6], u16, &[u8])> {
    if frame.len() < ETH_HDR_LEN { return None; }
    let mut dst = [0u8; 6];
    let mut src = [0u8; 6];
    dst.copy_from_slice(&frame[0..6]);
    src.copy_from_slice(&frame[6..12]);
    let etype = u16::from_be_bytes(frame[12..14].try_into().unwrap());
    Some((dst, src, etype, &frame[14..]))
}
