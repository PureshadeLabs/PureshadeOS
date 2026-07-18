//! The userspace protocol stack: Ethernet demux → ARP / IPv4 → ICMP echo.
//!
//! All protocol knowledge lives here (and its submodules) — the kernel mediates
//! only MMIO/IRQ/DMA and never parses a packet. [`NetStack`] owns the static IP
//! configuration and the ARP cache, and drives everything from a single inbound
//! entry point, [`NetStack::on_frame`], plus the outbound probe helpers used by
//! the boot-time round-trip test.

pub mod arp;
pub mod eth;
pub mod icmp;
pub mod ipv4;
pub mod wire;

use lythos_rt::println;

use arp::ArpCache;
use wire::{FrameSink, ETHERTYPE_ARP, ETHERTYPE_IPV4, ETH_HDR_LEN, MAC_BROADCAST};

/// Scratch buffer size for building an outbound IPv4/ICMP frame (well over the
/// 1500-byte MTU we ever emit here).
const TX_SCRATCH: usize = 1600;

/// Static interface configuration (no DHCP at this stage).
#[derive(Clone, Copy)]
pub struct Config {
    pub mac: [u8; 6],
    pub ip: [u8; 4],
    pub netmask: [u8; 4],
    pub gateway: [u8; 4],
}

/// The netd protocol stack.
pub struct NetStack {
    pub cfg: Config,
    arp: ArpCache,
    ip_id: u16,
    /// Last ICMP echo reply seen `(id, seq)` — the outbound probe watches this.
    pub last_echo_reply: Option<(u16, u16)>,
    /// Count of inbound echo requests we have answered.
    pub answered_pings: u32,
}

impl NetStack {
    pub fn new(cfg: Config) -> Self {
        Self {
            cfg,
            arp: ArpCache::new(),
            ip_id: 1,
            last_echo_reply: None,
            answered_pings: 0,
        }
    }

    fn next_ip_id(&mut self) -> u16 {
        let id = self.ip_id;
        self.ip_id = self.ip_id.wrapping_add(1);
        id
    }

    /// Resolve a live ARP binding for `ip`.
    pub fn arp_lookup(&self, ip: [u8; 4], now: u64) -> Option<[u8; 6]> {
        self.arp.lookup(ip, now)
    }

    /// Next-hop for `dst`: the destination itself when on-link, else the gateway.
    fn next_hop(&self, dst: [u8; 4]) -> [u8; 4] {
        let on_link = (0..4).all(|i| dst[i] & self.cfg.netmask[i] == self.cfg.ip[i] & self.cfg.netmask[i]);
        if on_link {
            dst
        } else {
            self.cfg.gateway
        }
    }

    /// True if `dst` is addressed to this interface (its IP or a broadcast).
    fn addressed_to_us(&self, dst: [u8; 4]) -> bool {
        if dst == self.cfg.ip || dst == [255, 255, 255, 255] {
            return true;
        }
        let mut subnet_bcast = [0u8; 4];
        for i in 0..4 {
            subnet_bcast[i] = self.cfg.ip[i] | !self.cfg.netmask[i];
        }
        dst == subnet_bcast
    }

    // ── Inbound ────────────────────────────────────────────────────────────

    /// Process one received Ethernet frame, emitting any reply through `sink`.
    /// Malformed frames are dropped silently; netd never trusts them.
    pub fn on_frame(&mut self, frame: &[u8], sink: &mut dyn FrameSink, now: u64) {
        let f = match eth::parse(frame) {
            Some(f) => f,
            None => return,
        };
        // Accept only frames addressed to us or broadcast (the NIC is not in
        // promiscuous mode, but filter defensively).
        if f.dst != self.cfg.mac && f.dst != MAC_BROADCAST {
            return;
        }
        match f.ethertype {
            ETHERTYPE_ARP => self.on_arp(f.payload, sink, now),
            ETHERTYPE_IPV4 => self.on_ipv4(f.src, f.payload, sink, now),
            _ => {} // not ARP or IPv4 — drop
        }
    }

    fn on_arp(&mut self, body: &[u8], sink: &mut dyn FrameSink, now: u64) {
        let a = match arp::parse(body) {
            Some(a) => a,
            None => return,
        };
        // Learn the sender's binding regardless of operation.
        if a.spa != [0, 0, 0, 0] {
            self.arp.insert(a.spa, a.sha, now);
        }
        // Answer requests for our own IP.
        if a.oper == arp::OP_REQUEST && a.tpa == self.cfg.ip {
            let mut buf = [0u8; 64];
            let n = arp::build(
                &mut buf,
                arp::OP_REPLY,
                &a.sha,
                &self.cfg.mac,
                &self.cfg.ip,
                &a.sha,
                &a.spa,
            );
            sink.send_frame(&buf[..n]);
            println!(
                "[netd] ARP who-has {}.{}.{}.{} tell {}.{}.{}.{} -> is-at {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
                a.tpa[0], a.tpa[1], a.tpa[2], a.tpa[3],
                a.spa[0], a.spa[1], a.spa[2], a.spa[3],
                self.cfg.mac[0], self.cfg.mac[1], self.cfg.mac[2],
                self.cfg.mac[3], self.cfg.mac[4], self.cfg.mac[5]
            );
        }
    }

    fn on_ipv4(&mut self, eth_src: [u8; 6], body: &[u8], sink: &mut dyn FrameSink, now: u64) {
        let pkt = match ipv4::parse(body) {
            Some(p) => p,
            None => return,
        };
        // Opportunistically learn src IP↔MAC from any IPv4 traffic.
        self.arp.insert(pkt.src, eth_src, now);
        // No forwarding: only packets addressed to us are processed.
        if !self.addressed_to_us(pkt.dst) {
            return;
        }
        if pkt.protocol == ipv4::PROTO_ICMP {
            self.on_icmp(pkt.src, eth_src, pkt.payload, sink, now);
        }
        // Other L4 protocols (UDP/TCP) are out of scope for this stage — drop.
    }

    fn on_icmp(
        &mut self,
        src_ip: [u8; 4],
        src_mac: [u8; 6],
        msg: &[u8],
        sink: &mut dyn FrameSink,
        _now: u64,
    ) {
        let echo = match icmp::parse(msg) {
            Some(e) => e,
            None => return,
        };
        match echo.kind {
            icmp::ECHO_REQUEST => {
                // Reply straight back to the requester's MAC.
                let mut buf = [0u8; TX_SCRATCH];
                let ident = self.next_ip_id();
                let n = build_ipv4_icmp(
                    &mut buf,
                    &src_mac,
                    &self.cfg.mac,
                    &self.cfg.ip,
                    &src_ip,
                    icmp::ECHO_REPLY,
                    echo.id,
                    echo.seq,
                    echo.data,
                    ident,
                );
                sink.send_frame(&buf[..n]);
                self.answered_pings = self.answered_pings.wrapping_add(1);
                println!(
                    "[netd] ICMP echo request from {}.{}.{}.{} id={} seq={} -> echo reply sent",
                    src_ip[0], src_ip[1], src_ip[2], src_ip[3], echo.id, echo.seq
                );
            }
            icmp::ECHO_REPLY => {
                self.last_echo_reply = Some((echo.id, echo.seq));
            }
            _ => {}
        }
    }

    // ── Outbound probe helpers ───────────────────────────────────────────────

    /// Broadcast an ARP request resolving `target_ip`.
    pub fn send_arp_request(&mut self, target_ip: [u8; 4], sink: &mut dyn FrameSink) {
        let mut buf = [0u8; 64];
        let n = arp::build_request(&mut buf, &self.cfg.mac, &self.cfg.ip, &target_ip);
        sink.send_frame(&buf[..n]);
    }

    /// Send an ICMP echo request to `dst_ip`. Returns `false` if the next-hop MAC
    /// is not yet in the ARP cache (resolve it first).
    pub fn send_icmp_echo(
        &mut self,
        dst_ip: [u8; 4],
        id: u16,
        seq: u16,
        sink: &mut dyn FrameSink,
        now: u64,
    ) -> bool {
        let next_hop = self.next_hop(dst_ip);
        let mac = match self.arp.lookup(next_hop, now) {
            Some(m) => m,
            None => return false,
        };
        // 32-byte incrementing payload, echoed back verbatim by the peer.
        let mut data = [0u8; 32];
        for (i, b) in data.iter_mut().enumerate() {
            *b = i as u8;
        }
        let mut buf = [0u8; TX_SCRATCH];
        let ident = self.next_ip_id();
        let n = build_ipv4_icmp(
            &mut buf,
            &mac,
            &self.cfg.mac,
            &self.cfg.ip,
            &dst_ip,
            icmp::ECHO_REQUEST,
            id,
            seq,
            &data,
            ident,
        );
        sink.send_frame(&buf[..n]);
        true
    }
}

/// Build a complete Ethernet → IPv4 → ICMP-echo frame into `buf`, returning its
/// length. Shared by the inbound reply path, the outbound probe, and the
/// in-guest self-test so the exact same emit code is exercised everywhere.
#[allow(clippy::too_many_arguments)]
pub fn build_ipv4_icmp(
    buf: &mut [u8],
    eth_dst: &[u8; 6],
    eth_src: &[u8; 6],
    src_ip: &[u8; 4],
    dst_ip: &[u8; 4],
    icmp_kind: u8,
    id: u16,
    seq: u16,
    data: &[u8],
    ident: u16,
) -> usize {
    eth::write_hdr(buf, eth_dst, eth_src, ETHERTYPE_IPV4);
    let ip_off = ETH_HDR_LEN;
    let icmp_off = ip_off + ipv4::HDR_LEN;
    let icmp_len = icmp::write_echo(&mut buf[icmp_off..], icmp_kind, id, seq, data);
    ipv4::write_header(
        &mut buf[ip_off..],
        src_ip,
        dst_ip,
        ipv4::PROTO_ICMP,
        icmp_len,
        ident,
    );
    icmp_off + icmp_len
}
