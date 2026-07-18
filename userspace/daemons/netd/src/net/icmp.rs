//! ICMP echo (RFC 792): parse an echo header and build echo request/reply
//! messages with a correct ICMP checksum (computed over the whole ICMP message).

use super::wire::{be16, checksum, wr16};

pub const ECHO_REPLY: u8 = 0;
pub const ECHO_REQUEST: u8 = 8;

/// ICMP header length for echo (type/code/checksum/id/seq).
pub const HDR_LEN: usize = 8;

/// A parsed ICMP echo message (type validated by the caller).
pub struct Echo<'a> {
    pub kind: u8,
    pub id: u16,
    pub seq: u16,
    pub data: &'a [u8],
}

/// Parse an ICMP message and verify its checksum. Returns `None` (drop) if the
/// message is too short or the checksum is wrong.
pub fn parse(msg: &[u8]) -> Option<Echo<'_>> {
    if msg.len() < HDR_LEN {
        return None;
    }
    if checksum(msg) != 0 {
        return None; // corrupt ICMP message
    }
    Some(Echo {
        kind: msg[0],
        id: be16(&msg[4..6]),
        seq: be16(&msg[6..8]),
        data: &msg[HDR_LEN..],
    })
}

/// Write an ICMP echo message (`kind` = request/reply) into `buf`, appending
/// `data`, and fill in the checksum. Returns the message length. `buf` must hold
/// `8 + data.len()` bytes.
pub fn write_echo(buf: &mut [u8], kind: u8, id: u16, seq: u16, data: &[u8]) -> usize {
    buf[0] = kind;
    buf[1] = 0; // code
    wr16(&mut buf[2..4], 0); // checksum placeholder
    wr16(&mut buf[4..6], id);
    wr16(&mut buf[6..8], seq);
    let n = data.len().min(buf.len() - HDR_LEN);
    buf[HDR_LEN..HDR_LEN + n].copy_from_slice(&data[..n]);
    let len = HDR_LEN + n;
    let ck = checksum(&buf[..len]);
    wr16(&mut buf[2..4], ck);
    len
}
