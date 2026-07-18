//! Ethernet II framing: parse the 14-byte header and demux by ethertype; write a
//! header for outbound frames. No 802.1Q/VLAN, no SNAP — plain Ethernet II.

use super::wire::{be16, wr16, ETH_HDR_LEN};

/// A parsed Ethernet II frame borrowing the receive buffer.
pub struct EthFrame<'a> {
    pub dst: [u8; 6],
    pub src: [u8; 6],
    pub ethertype: u16,
    pub payload: &'a [u8],
}

/// Parse an Ethernet II frame, or `None` if it is too short to hold a header.
pub fn parse(frame: &[u8]) -> Option<EthFrame<'_>> {
    if frame.len() < ETH_HDR_LEN {
        return None;
    }
    let mut dst = [0u8; 6];
    let mut src = [0u8; 6];
    dst.copy_from_slice(&frame[0..6]);
    src.copy_from_slice(&frame[6..12]);
    Some(EthFrame {
        dst,
        src,
        ethertype: be16(&frame[12..14]),
        payload: &frame[ETH_HDR_LEN..],
    })
}

/// Write a 14-byte Ethernet II header into `buf` (caller guarantees len ≥ 14).
pub fn write_hdr(buf: &mut [u8], dst: &[u8; 6], src: &[u8; 6], ethertype: u16) {
    buf[0..6].copy_from_slice(dst);
    buf[6..12].copy_from_slice(src);
    wr16(&mut buf[12..14], ethertype);
}
