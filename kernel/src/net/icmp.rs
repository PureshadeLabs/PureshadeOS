//! ICMP — echo request/reply (ping).

use super::{eth, ip};

const ICMP_ECHO_REQUEST: u8 = 8;
const ICMP_ECHO_REPLY:   u8 = 0;

/// Handle an incoming ICMP packet addressed to `our_ip`.
///
/// If it's an echo request, send an echo reply.
pub fn handle(our_mac: &[u8; 6], our_ip: u32, src_mac: &[u8; 6], src_ip: u32, pkt: &[u8]) {
    if pkt.len() < 4 { return; }
    let typ = pkt[0];
    if typ != ICMP_ECHO_REQUEST { return; }

    // Build reply: same payload, type=0.
    let payload_len = pkt.len();
    let total_len = eth::ETH_HDR_LEN + ip::IP_HDR_LEN + payload_len;
    if total_len > crate::virtio_net::MAX_FRAME + eth::ETH_HDR_LEN { return; }

    let mut frame = [0u8; crate::virtio_net::MAX_FRAME + eth::ETH_HDR_LEN];
    let mut ip_hdr = [0u8; ip::IP_HDR_LEN];
    ip::build(&mut ip_hdr, ip::proto::ICMP, our_ip, src_ip, payload_len as u16, 1);

    // Copy ICMP with type=reply, zero out checksum field, recompute.
    let icmp_start = eth::ETH_HDR_LEN + ip::IP_HDR_LEN;
    frame[icmp_start..icmp_start + payload_len].copy_from_slice(pkt);
    frame[icmp_start] = ICMP_ECHO_REPLY;
    frame[icmp_start + 1] = 0; // code
    frame[icmp_start + 2] = 0; // checksum hi
    frame[icmp_start + 3] = 0; // checksum lo

    let icmp_csum = ip::checksum(&frame[icmp_start..icmp_start + payload_len]);
    frame[icmp_start + 2..icmp_start + 4].copy_from_slice(&icmp_csum.to_be_bytes());

    // Build properly — eth::build clobbers the slice it writes to, so use separate buffers.
    let mut out = [0u8; crate::virtio_net::MAX_FRAME + eth::ETH_HDR_LEN];
    let mut inner = [0u8; crate::virtio_net::MAX_FRAME];
    inner[..ip::IP_HDR_LEN].copy_from_slice(&ip_hdr);
    inner[ip::IP_HDR_LEN] = ICMP_ECHO_REPLY;
    inner[ip::IP_HDR_LEN + 1] = 0;
    inner[ip::IP_HDR_LEN + 2] = 0;
    inner[ip::IP_HDR_LEN + 3] = 0;
    inner[ip::IP_HDR_LEN + 4..ip::IP_HDR_LEN + payload_len].copy_from_slice(&pkt[4..]);
    let icmp_csum = ip::checksum(&inner[ip::IP_HDR_LEN..ip::IP_HDR_LEN + payload_len]);
    inner[ip::IP_HDR_LEN + 2..ip::IP_HDR_LEN + 4].copy_from_slice(&icmp_csum.to_be_bytes());

    let total = eth::build(&mut out, src_mac, our_mac, eth::ethertype::IPV4,
                           &inner[..ip::IP_HDR_LEN + payload_len]);
    crate::virtio_net::send(&out[..total]);
}
