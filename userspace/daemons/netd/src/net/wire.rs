//! Wire primitives shared by every protocol layer: big-endian field access, the
//! Internet ones-complement checksum (RFC 1071), and the [`FrameSink`] trait
//! that decouples emit from its destination — the NIC on the wire, or a capture
//! buffer for the in-guest self-tests.

/// Ethernet II header length (dst[6] + src[6] + ethertype[2]).
pub const ETH_HDR_LEN: usize = 14;
/// Minimum Ethernet payload+header on the wire (padding target).
pub const ETH_MIN_FRAME: usize = 60;

pub const ETHERTYPE_ARP: u16 = 0x0806;
pub const ETHERTYPE_IPV4: u16 = 0x0800;

pub const MAC_BROADCAST: [u8; 6] = [0xFF; 6];

/// Read a big-endian u16 from the first two bytes of `b` (caller guarantees len ≥ 2).
#[inline]
pub fn be16(b: &[u8]) -> u16 {
    ((b[0] as u16) << 8) | b[1] as u16
}

/// Write `v` big-endian into the first two bytes of `b` (caller guarantees len ≥ 2).
#[inline]
pub fn wr16(b: &mut [u8], v: u16) {
    b[0] = (v >> 8) as u8;
    b[1] = v as u8;
}

/// The Internet checksum: ones-complement sum of 16-bit big-endian words.
///
/// Verifying a block that already carries its checksum yields 0 when intact.
pub fn checksum(data: &[u8]) -> u16 {
    let mut sum: u32 = 0;
    let mut i = 0;
    while i + 1 < data.len() {
        sum += be16(&data[i..]) as u32;
        i += 2;
    }
    if i < data.len() {
        // Odd trailing byte is the high half of a 16-bit word.
        sum += (data[i] as u32) << 8;
    }
    while sum >> 16 != 0 {
        sum = (sum & 0xFFFF) + (sum >> 16);
    }
    !(sum as u16)
}

/// A destination for an outbound Ethernet frame. `Nic` sends it on the wire; the
/// self-test `CaptureSink` records it. Protocol code emits through this trait so
/// the identical build path is exercised on the wire and in-guest.
pub trait FrameSink {
    /// Transmit one complete Ethernet frame (starting at the dst MAC, no virtio
    /// header — the NIC prepends that).
    fn send_frame(&mut self, frame: &[u8]);
}
