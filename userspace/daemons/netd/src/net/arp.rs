//! ARP for IPv4-over-Ethernet (RFC 826): a small expiring IP↔MAC cache, request
//! parsing/emit, and reply emit. Only htype=Ethernet(1)/ptype=IPv4(0x0800) with
//! 6-byte hardware and 4-byte protocol addresses are handled; anything else is
//! dropped by the parser.

use super::eth;
use super::wire::{be16, wr16, ETHERTYPE_ARP, ETH_HDR_LEN, ETH_MIN_FRAME, MAC_BROADCAST};

pub const OP_REQUEST: u16 = 1;
pub const OP_REPLY: u16 = 2;

/// ARP body length for Ethernet/IPv4 (fixed 28 bytes).
const ARP_BODY_LEN: usize = 28;

/// Cache entry lifetime: a resolved binding is trusted for this long.
const ENTRY_TTL_MS: u64 = 60_000;
/// Cache capacity (gateway + a handful of local peers is plenty here).
const CACHE_SLOTS: usize = 8;

/// A parsed ARP packet (Ethernet/IPv4 flavour).
pub struct ArpPacket {
    pub oper: u16,
    pub sha: [u8; 6],
    pub spa: [u8; 4],
    /// Target hardware address (parsed for completeness; unused by our logic).
    #[allow(dead_code)]
    pub tha: [u8; 6],
    pub tpa: [u8; 4],
}

/// Parse an ARP body, rejecting non-Ethernet/IPv4 packets.
pub fn parse(p: &[u8]) -> Option<ArpPacket> {
    if p.len() < ARP_BODY_LEN {
        return None;
    }
    if be16(&p[0..2]) != 1 || be16(&p[2..4]) != 0x0800 || p[4] != 6 || p[5] != 4 {
        return None; // not Ethernet/IPv4 ARP
    }
    let mut sha = [0u8; 6];
    let mut spa = [0u8; 4];
    let mut tha = [0u8; 6];
    let mut tpa = [0u8; 4];
    sha.copy_from_slice(&p[8..14]);
    spa.copy_from_slice(&p[14..18]);
    tha.copy_from_slice(&p[18..24]);
    tpa.copy_from_slice(&p[24..28]);
    Some(ArpPacket {
        oper: be16(&p[6..8]),
        sha,
        spa,
        tha,
        tpa,
    })
}

/// Build a complete ARP Ethernet frame (header + 28-byte body, zero-padded to
/// the 60-byte Ethernet minimum). Returns the frame length. `buf` must be ≥ 60.
pub fn build(
    buf: &mut [u8],
    oper: u16,
    eth_dst: &[u8; 6],
    src_mac: &[u8; 6],
    src_ip: &[u8; 4],
    target_mac: &[u8; 6],
    target_ip: &[u8; 4],
) -> usize {
    for b in buf[..ETH_MIN_FRAME].iter_mut() {
        *b = 0;
    }
    eth::write_hdr(buf, eth_dst, src_mac, ETHERTYPE_ARP);
    let a = &mut buf[ETH_HDR_LEN..];
    wr16(&mut a[0..2], 1); // htype: Ethernet
    wr16(&mut a[2..4], 0x0800); // ptype: IPv4
    a[4] = 6; // hlen
    a[5] = 4; // plen
    wr16(&mut a[6..8], oper);
    a[8..14].copy_from_slice(src_mac);
    a[14..18].copy_from_slice(src_ip);
    a[18..24].copy_from_slice(target_mac);
    a[24..28].copy_from_slice(target_ip);
    ETH_MIN_FRAME
}

/// Convenience: build a broadcast ARP request asking who-has `target_ip`.
pub fn build_request(
    buf: &mut [u8],
    src_mac: &[u8; 6],
    src_ip: &[u8; 4],
    target_ip: &[u8; 4],
) -> usize {
    build(buf, OP_REQUEST, &MAC_BROADCAST, src_mac, src_ip, &[0u8; 6], target_ip)
}

// ── Cache ───────────────────────────────────────────────────────────────────

#[derive(Clone, Copy)]
struct Entry {
    ip: [u8; 4],
    mac: [u8; 6],
    expiry: u64,
    valid: bool,
}

/// A small fixed-capacity ARP cache with per-entry expiry.
pub struct ArpCache {
    entries: [Entry; CACHE_SLOTS],
}

impl ArpCache {
    pub const fn new() -> Self {
        Self {
            entries: [Entry {
                ip: [0; 4],
                mac: [0; 6],
                expiry: 0,
                valid: false,
            }; CACHE_SLOTS],
        }
    }

    /// Insert or refresh `ip → mac`, valid until `now + TTL`. Reuses an existing
    /// slot for the same IP, else the first free slot, else the oldest.
    pub fn insert(&mut self, ip: [u8; 4], mac: [u8; 6], now: u64) {
        let mut victim = 0usize;
        let mut oldest = u64::MAX;
        for (i, e) in self.entries.iter().enumerate() {
            if e.valid && e.ip == ip {
                victim = i;
                break;
            }
            if !e.valid {
                victim = i;
                break;
            }
            if e.expiry < oldest {
                oldest = e.expiry;
                victim = i;
            }
        }
        self.entries[victim] = Entry {
            ip,
            mac,
            expiry: now.saturating_add(ENTRY_TTL_MS),
            valid: true,
        };
    }

    /// Look up a live binding for `ip`, honouring expiry against `now`.
    pub fn lookup(&self, ip: [u8; 4], now: u64) -> Option<[u8; 6]> {
        for e in &self.entries {
            if e.valid && e.ip == ip && now < e.expiry {
                return Some(e.mac);
            }
        }
        None
    }
}
