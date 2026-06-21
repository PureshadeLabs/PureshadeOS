//! ARP (RFC 826) — request/reply for IPv4-over-Ethernet.

use super::eth;

const ARP_LEN: usize = 28;

pub const HW_ETHER:  u16 = 1;
pub const PTYPE_IP:  u16 = 0x0800;
pub const OP_REQ:    u16 = 1;
pub const OP_REPLY:  u16 = 2;

/// ARP cache entry.
#[derive(Clone, Copy)]
struct Entry {
    ip:  u32,
    mac: [u8; 6],
}

const CACHE_SIZE: usize = 32;
static ARP_CACHE: crate::serial::SpinLock<[Option<Entry>; CACHE_SIZE]> =
    crate::serial::SpinLock::new([None; CACHE_SIZE]);

/// Look up a MAC for `ip` in the cache.
pub fn lookup(ip: u32) -> Option<[u8; 6]> {
    let cache = ARP_CACHE.lock();
    cache.iter().find_map(|e| e.filter(|e| e.ip == ip).map(|e| e.mac))
}

/// Insert or update an ARP cache entry.
pub fn insert(ip: u32, mac: [u8; 6]) {
    let mut cache = ARP_CACHE.lock();
    // Update existing entry.
    for slot in cache.iter_mut() {
        if let Some(e) = slot {
            if e.ip == ip { e.mac = mac; return; }
        }
    }
    // Find empty slot.
    for slot in cache.iter_mut() {
        if slot.is_none() { *slot = Some(Entry { ip, mac }); return; }
    }
    // Evict first entry (simple LRU approximation).
    cache[0] = Some(Entry { ip, mac });
}

/// Build an ARP packet into `buf`. Returns the ARP payload length (28).
pub fn build(buf: &mut [u8; ARP_LEN], op: u16,
             sender_mac: &[u8; 6], sender_ip: u32,
             target_mac: &[u8; 6], target_ip: u32) {
    buf[0..2].copy_from_slice(&HW_ETHER.to_be_bytes());
    buf[2..4].copy_from_slice(&PTYPE_IP.to_be_bytes());
    buf[4] = 6; // hardware address length
    buf[5] = 4; // protocol address length
    buf[6..8].copy_from_slice(&op.to_be_bytes());
    buf[8..14].copy_from_slice(sender_mac);
    buf[14..18].copy_from_slice(&sender_ip.to_be_bytes());
    buf[18..24].copy_from_slice(target_mac);
    buf[24..28].copy_from_slice(&target_ip.to_be_bytes());
}

/// Parse an ARP payload. Returns (op, sender_mac, sender_ip, target_mac, target_ip).
pub fn parse(pkt: &[u8]) -> Option<(u16, [u8; 6], u32, [u8; 6], u32)> {
    if pkt.len() < ARP_LEN { return None; }
    let op = u16::from_be_bytes([pkt[6], pkt[7]]);
    let mut smac = [0u8; 6]; smac.copy_from_slice(&pkt[8..14]);
    let sip = u32::from_be_bytes(pkt[14..18].try_into().unwrap());
    let mut tmac = [0u8; 6]; tmac.copy_from_slice(&pkt[18..24]);
    let tip = u32::from_be_bytes(pkt[24..28].try_into().unwrap());
    Some((op, smac, sip, tmac, tip))
}

/// Send an ARP request for `target_ip`.
pub fn send_request(our_mac: &[u8; 6], our_ip: u32, target_ip: u32) {
    let mut arp = [0u8; ARP_LEN];
    build(&mut arp, OP_REQ, our_mac, our_ip, &[0u8; 6], target_ip);

    let mut frame = [0u8; eth::ETH_HDR_LEN + ARP_LEN];
    eth::build(&mut frame, &eth::MAC_BROADCAST, our_mac, eth::ethertype::ARP, &arp);
    crate::virtio_net::send(&frame[..eth::ETH_HDR_LEN + ARP_LEN]);
}

/// Send an ARP reply in response to an incoming request.
pub fn send_reply(our_mac: &[u8; 6], our_ip: u32,
                  req_mac: &[u8; 6], req_ip: u32) {
    let mut arp = [0u8; ARP_LEN];
    build(&mut arp, OP_REPLY, our_mac, our_ip, req_mac, req_ip);

    let mut frame = [0u8; eth::ETH_HDR_LEN + ARP_LEN];
    eth::build(&mut frame, req_mac, our_mac, eth::ethertype::ARP, &arp);
    crate::virtio_net::send(&frame[..eth::ETH_HDR_LEN + ARP_LEN]);
}

/// Process an incoming ARP packet.
pub fn handle(our_mac: &[u8; 6], our_ip: u32, pkt: &[u8]) {
    let (op, smac, sip, _tmac, tip) = match parse(pkt) { Some(v) => v, None => return };
    insert(sip, smac);
    if op == OP_REQ && tip == our_ip {
        send_reply(our_mac, our_ip, &smac, sip);
    }
}
