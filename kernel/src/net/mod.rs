//! Lythos in-kernel TCP/IP stack.
//!
//! ## Static network configuration (QEMU user networking)
//!
//! | Parameter  | Value         |
//! |------------|---------------|
//! | VM IP      | 10.0.2.15     |
//! | Netmask    | 255.255.255.0 |
//! | Gateway    | 10.0.2.2      |
//!
//! ## Architecture
//!
//! A kernel task (`net_task`) loops calling `crate::virtio_net::try_recv` and
//! dispatching to ARP / ICMP / UDP handlers.  Blocking socket operations
//! suspend the calling task using the scheduler primitives.
//!
//! ## Syscall numbers (socket API)
//!
//! | Nr | Name          | Args                                   |
//! |----|---------------|----------------------------------------|
//! | 50 | SYS_SOCKET    | —                                      |
//! | 51 | SYS_BIND      | fd, port                               |
//! | 52 | SYS_SENDTO    | fd, buf_ptr, len, dst_ip, dst_port     |
//! | 53 | SYS_RECVFROM  | fd, buf_ptr, len, ip_out_ptr, port_out |

pub mod arp;
pub mod eth;
pub mod icmp;
pub mod ip;
pub mod udp;

use core::cell::UnsafeCell;

// ── Static network configuration ─────────────────────────────────────────────

/// QEMU user networking default VM IP (10.0.2.15).
pub const OUR_IP: u32 = 0x0A00_020F;

struct NetState(UnsafeCell<Option<[u8; 6]>>);
unsafe impl Sync for NetState {}
static MAC: NetState = NetState(UnsafeCell::new(None));

pub fn our_mac() -> [u8; 6] {
    unsafe { (*MAC.0.get()).unwrap_or([0u8; 6]) }
}

/// Initialise the network stack. Must be called after `virtio_net::init()`.
pub fn init() {
    let mac = crate::virtio_net::mac_addr();
    unsafe { *MAC.0.get() = Some(mac); }

    // Pre-populate ARP cache with the gateway.
    // (Will be populated dynamically on first packet from gateway.)
    // Spawn the net processing task.
    crate::task::spawn_kernel_task(net_task);
}

/// Kernel task: polls the virtio-net RX ring and dispatches packets.
fn net_task() -> ! {
    let mut buf = [0u8; crate::virtio_net::MAX_FRAME];
    loop {
        if let Some(n) = crate::virtio_net::try_recv(&mut buf) {
            process_frame(&buf[..n]);
        } else {
            crate::task::yield_task();
        }
    }
}

/// Dispatch a single received Ethernet frame.
fn process_frame(frame: &[u8]) {
    let (dst_mac, src_mac, etype, payload) = match eth::parse(frame) {
        Some(v) => v,
        None    => return,
    };
    let our_mac = our_mac();

    // Accept broadcasts and frames addressed to us.
    let for_us = dst_mac == our_mac || dst_mac == eth::MAC_BROADCAST;
    if !for_us { return; }

    match etype {
        eth::ethertype::ARP => arp::handle(&our_mac, OUR_IP, payload),
        eth::ethertype::IPV4 => {
            let (src_ip, dst_ip, proto, ip_payload) = match ip::parse(payload) {
                Some(v) => v,
                None    => return,
            };
            // Learn the sender's MAC from IP traffic too.
            arp::insert(src_ip, src_mac);

            if dst_ip != OUR_IP { return; }

            match proto {
                ip::proto::ICMP => icmp::handle(&our_mac, OUR_IP, &src_mac, src_ip, ip_payload),
                ip::proto::UDP  => udp::handle(src_ip, dst_ip, ip_payload),
                _               => {}
            }
        }
        _ => {}
    }
}
