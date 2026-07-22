//! netd — userspace virtio-net driver + protocol stack (stage 1).
//!
//! netd is spawned by lythd with a single capability at handle 0: the Device
//! capability for the virtio-net PCI device. Holding only that cap — no ambient
//! authority — it brings the device to virtqueue-operational ([`nic::Nic`]) and
//! then runs a userspace Ethernet/ARP/IPv4/ICMP stack ([`net::NetStack`]).
//!
//! This stage reaches the first packet round-trip: netd ARP-resolves the QEMU
//! SLIRP gateway, pings it (ICMP echo request → reply), and thereafter answers
//! inbound ARP requests and ICMP echo requests for its static IP. No DHCP, DNS,
//! UDP, TCP, or socket ABI — those are later stages. All protocol logic lives in
//! userspace; the kernel mediates only MMIO/IRQ/DMA.

#![no_std]
#![no_main]

mod net;
mod nic;

use lythos_rt::{eprintln, println, sys_task_exit, sys_time, task::yield_now};

use net::wire::{checksum, FrameSink};
use net::{build_ipv4_icmp, icmp, Config, NetStack};
use nic::Nic;

// ── Static interface configuration (QEMU user-net / SLIRP) ──────────────────
// SLIRP hands out 10.0.2.0/24 with the gateway at 10.0.2.2; the guest normally
// takes 10.0.2.15 via DHCP. We assign it statically (no DHCP this stage).
const OUR_IP: [u8; 4] = [10, 0, 2, 15];
const NETMASK: [u8; 4] = [255, 255, 255, 0];
const GATEWAY: [u8; 4] = [10, 0, 2, 2];

/// ICMP identifier for our outbound probe echoes.
const PROBE_ID: u16 = 0x1D01;

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    if let Err(e) = run() {
        eprintln!("[netd] FAILED: {}", e);
    }
    sys_task_exit(0)
}

fn run() -> Result<(), &'static str> {
    let mut nic = Nic::bringup()?;
    let cfg = Config {
        mac: nic.mac,
        ip: OUR_IP,
        netmask: NETMASK,
        gateway: GATEWAY,
    };
    println!(
        "[netd] MAC {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x} | ip 10.0.2.15/24 gw 10.0.2.2 (static)",
        cfg.mac[0], cfg.mac[1], cfg.mac[2], cfg.mac[3], cfg.mac[4], cfg.mac[5]
    );
    let mut stack = NetStack::new(cfg);

    // ── Milestone probe: resolve + ping the gateway ─────────────────────────
    match probe(&mut nic, &mut stack) {
        Ok(()) => {}
        Err(e) => eprintln!("[netd] probe: {} (continuing to service loop)", e),
    }

    // ── In-guest inbound-ping self-test (SLIRP cannot route host ICMP to the
    //    guest IP, so we verify the answer path with a crafted local frame) ──
    selftest_inbound(&mut stack);

    // ── Service loop: answer inbound ARP + ICMP, IRQ-driven ─────────────────
    service_loop(&mut nic, &mut stack)
}

/// The milestone: ARP-resolve the gateway, then ICMP-echo it and match the reply.
fn probe(nic: &mut Nic, stack: &mut NetStack) -> Result<(), &'static str> {
    let gw_mac = resolve(nic, stack, GATEWAY)?;
    println!(
        "[netd] probe: ARP resolve 10.0.2.2 -> {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        gw_mac[0], gw_mac[1], gw_mac[2], gw_mac[3], gw_mac[4], gw_mac[5]
    );

    let seq = 1u16;
    let t0 = sys_time();
    if !stack.send_icmp_echo(GATEWAY, PROBE_ID, seq, nic, t0) {
        return Err("gateway MAC unresolved at echo time");
    }
    println!("[netd] probe: ICMP echo request -> 10.0.2.2 id={} seq={}", PROBE_ID, seq);

    let mut frame = [0u8; 2048];
    for _ in 0..8 {
        if matches!(stack.last_echo_reply, Some((id, s)) if id == PROBE_ID && s == seq) {
            break;
        }
        nic.wait_irq()?;
        while let Some(len) = nic.recv_into(&mut frame) {
            stack.on_frame(&frame[..len], nic, sys_time());
        }
        nic.ack_irq();
    }
    match stack.last_echo_reply {
        Some((id, s)) if id == PROBE_ID && s == seq => {
            let rtt = sys_time().saturating_sub(t0);
            println!(
                "[netd] probe: ICMP echo reply <- 10.0.2.2 id={} seq={} rtt={}ms — ROUND-TRIP OK",
                id, s, rtt
            );
            Ok(())
        }
        _ => Err("no ICMP echo reply from gateway"),
    }
}

/// ARP-resolve `ip`, driving the ring-3 IRQ path. Retransmits the request a few
/// times in case the first is lost; returns the resolved MAC.
fn resolve(nic: &mut Nic, stack: &mut NetStack, ip: [u8; 4]) -> Result<[u8; 6], &'static str> {
    let mut frame = [0u8; 2048];
    for _attempt in 0..5 {
        stack.send_arp_request(ip, nic);
        for _ in 0..6 {
            if let Some(mac) = stack.arp_lookup(ip, sys_time()) {
                return Ok(mac);
            }
            nic.wait_irq()?;
            while let Some(len) = nic.recv_into(&mut frame) {
                stack.on_frame(&frame[..len], nic, sys_time());
            }
            nic.ack_irq();
        }
    }
    stack
        .arp_lookup(ip, sys_time())
        .ok_or("ARP resolution timed out")
}

/// Answer inbound ARP + ICMP indefinitely, blocking on the device IRQ.
fn service_loop(nic: &mut Nic, stack: &mut NetStack) -> Result<(), &'static str> {
    println!("[netd] entering service loop — answering ARP + ICMP echo for 10.0.2.15");
    let mut frame = [0u8; 2048];
    loop {
        if nic.wait_irq().is_err() {
            yield_now();
            continue;
        }
        while let Some(len) = nic.recv_into(&mut frame) {
            stack.on_frame(&frame[..len], nic, sys_time());
        }
        nic.ack_irq();
    }
}

/// A [`FrameSink`] that records the last emitted frame instead of sending it —
/// used by the in-guest self-test to inspect the reply netd would transmit.
struct CaptureSink {
    buf: [u8; 1600],
    len: usize,
    count: u32,
}

impl CaptureSink {
    fn new() -> Self {
        Self { buf: [0u8; 1600], len: 0, count: 0 }
    }
}

impl FrameSink for CaptureSink {
    fn send_frame(&mut self, frame: &[u8]) {
        let n = frame.len().min(self.buf.len());
        self.buf[..n].copy_from_slice(&frame[..n]);
        self.len = n;
        self.count = self.count.wrapping_add(1);
    }
}

/// Verify the inbound-ping answer path in-guest: craft an ICMP echo request from
/// the gateway to our IP, run it through the real `on_frame` path, and check the
/// captured reply is a well-formed echo reply with correct checksums. Also feed a
/// truncated frame to confirm malformed input is dropped without a reply.
fn selftest_inbound(stack: &mut NetStack) {
    const PEER_MAC: [u8; 6] = [0x52, 0x55, 0x0a, 0x00, 0x02, 0x02];
    let now = sys_time();

    // Craft an echo request: PEER (gateway) -> us.
    let mut req = [0u8; 1600];
    let data = [0xA5u8; 32];
    let n = build_ipv4_icmp(
        &mut req,
        &stack.cfg.mac, // eth dst = us
        &PEER_MAC,      // eth src = gateway
        &GATEWAY,       // ip src
        &OUR_IP,        // ip dst = us
        icmp::ECHO_REQUEST,
        0x4242,
        7,
        &data,
        0x99,
    );

    let mut cap = CaptureSink::new();
    stack.on_frame(&req[..n], &mut cap, now);

    let ok = cap.count == 1 && reply_is_valid_echo(&cap.buf[..cap.len]);
    println!(
        "[netd] self-test inbound-ping (in-guest capture probe): {}",
        if ok { "PASS — echo reply built, checksums valid" } else { "FAIL" }
    );

    // Malformed: an IPv4 frame claiming IHL but truncated mid-header.
    let mut bad = [0u8; 1600];
    net::eth::write_hdr(&mut bad, &stack.cfg.mac, &PEER_MAC, net::wire::ETHERTYPE_IPV4);
    bad[net::wire::ETH_HDR_LEN] = 0x45; // version/IHL, but only a few bytes follow
    let mut cap2 = CaptureSink::new();
    stack.on_frame(&bad[..net::wire::ETH_HDR_LEN + 4], &mut cap2, now);
    println!(
        "[netd] self-test malformed-drop: {}",
        if cap2.count == 0 { "PASS — dropped, no reply, no crash" } else { "FAIL" }
    );
}

/// Validate that `frame` is a complete Ethernet/IPv4/ICMP echo reply with intact
/// IPv4 and ICMP checksums.
fn reply_is_valid_echo(frame: &[u8]) -> bool {
    let eth = match net::eth::parse(frame) {
        Some(e) => e,
        None => return false,
    };
    if eth.ethertype != net::wire::ETHERTYPE_IPV4 {
        return false;
    }
    let ip = match net::ipv4::parse(eth.payload) {
        Some(p) => p,
        None => return false, // parse re-verifies the IPv4 header checksum
    };
    if ip.protocol != net::ipv4::PROTO_ICMP {
        return false;
    }
    // icmp::parse re-verifies the ICMP checksum.
    match icmp::parse(ip.payload) {
        Some(e) => e.kind == icmp::ECHO_REPLY && checksum(ip.payload) == 0,
        None => false,
    }
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo<'_>) -> ! {
    lythos_rt::sys_log("[netd] PANIC");
    if let Some(msg) = info.message().as_str() {
        lythos_rt::sys_log(": ");
        lythos_rt::sys_log(msg);
    }
    lythos_rt::sys_log("\n");
    sys_task_exit(0)
}
