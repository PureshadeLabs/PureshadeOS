//! UDP socket table and datagram dispatch.

extern crate alloc;
use alloc::collections::VecDeque;
use alloc::vec::Vec;
use core::cell::UnsafeCell;

use super::{eth, ip};

const UDP_HDR_LEN: usize = 8;
#[allow(dead_code)]
const MAX_SOCKETS: usize = 64;
const RECV_QUEUE_DEPTH: usize = 32;

/// An incoming UDP datagram stored in a socket's receive queue.
pub struct Datagram {
    pub src_ip:   u32,
    pub src_port: u16,
    pub data:     Vec<u8>,
}

/// A UDP socket.
pub struct UdpSocket {
    pub id:         i64,
    pub local_port: u16,
    recv_q:         VecDeque<Datagram>,
    /// Task blocked waiting for a datagram, if any.
    pub waiter:     Option<crate::task::TaskId>,
}

struct SocketTable(UnsafeCell<Option<Vec<UdpSocket>>>);
unsafe impl Sync for SocketTable {}
static SOCKETS: SocketTable = SocketTable(UnsafeCell::new(None));

fn table() -> &'static mut Vec<UdpSocket> {
    unsafe {
        let t = &mut *SOCKETS.0.get();
        if t.is_none() { *t = Some(Vec::new()); }
        t.as_mut().unwrap()
    }
}

static NEXT_SOCK_ID: crate::serial::SpinLock<i64> = crate::serial::SpinLock::new(3); // 0/1/2 reserved

/// Create a new UDP socket. Returns socket fd (>= 0).
pub fn create() -> i64 {
    let id = {
        let mut n = NEXT_SOCK_ID.lock();
        let id = *n;
        *n += 1;
        id
    };
    table().push(UdpSocket {
        id,
        local_port: 0,
        recv_q:     VecDeque::new(),
        waiter:     None,
    });
    id
}

/// Bind a socket to a local port. Returns `true` on success.
pub fn bind(fd: i64, port: u16) -> bool {
    // Reject if port already in use.
    for s in table().iter() {
        if s.local_port == port && s.id != fd { return false; }
    }
    for s in table().iter_mut() {
        if s.id == fd { s.local_port = port; return true; }
    }
    false
}

/// Send a UDP datagram. Returns `true` on success.
pub fn send(
    our_mac: &[u8; 6], our_ip: u32,
    fd: i64,
    dst_ip: u32, dst_port: u16,
    data: &[u8],
) -> bool {
    let src_port = table().iter().find(|s| s.id == fd).map(|s| s.local_port).unwrap_or(0);
    if src_port == 0 { return false; }

    // Resolve destination MAC via ARP cache (or send ARP request).
    let dst_mac = match super::arp::lookup(dst_ip) {
        Some(m) => m,
        None    => {
            super::arp::send_request(our_mac, our_ip, dst_ip);
            return false; // caller should retry after ARP resolves
        }
    };

    let udp_len = UDP_HDR_LEN + data.len();
    let ip_payload_len = ip::IP_HDR_LEN + udp_len;
    if ip_payload_len > crate::virtio_net::MAX_FRAME { return false; }

    let mut inner = [0u8; crate::virtio_net::MAX_FRAME];
    // UDP header.
    inner[0..2].copy_from_slice(&src_port.to_be_bytes());
    inner[2..4].copy_from_slice(&dst_port.to_be_bytes());
    inner[4..6].copy_from_slice(&(udp_len as u16).to_be_bytes());
    inner[6..8].copy_from_slice(&0u16.to_be_bytes()); // no checksum (optional for IPv4)
    inner[8..8 + data.len()].copy_from_slice(data);

    let mut ip_hdr = [0u8; ip::IP_HDR_LEN];
    ip::build(&mut ip_hdr, ip::proto::UDP, our_ip, dst_ip, udp_len as u16, 1);

    let mut frame = [0u8; crate::virtio_net::MAX_FRAME + eth::ETH_HDR_LEN];
    let payload_len = ip::IP_HDR_LEN + udp_len;
    let mut payload = [0u8; crate::virtio_net::MAX_FRAME];
    payload[..ip::IP_HDR_LEN].copy_from_slice(&ip_hdr);
    payload[ip::IP_HDR_LEN..ip::IP_HDR_LEN + udp_len].copy_from_slice(&inner[..udp_len]);

    let total = eth::build(&mut frame, &dst_mac, our_mac, eth::ethertype::IPV4, &payload[..payload_len]);
    crate::virtio_net::send(&frame[..total])
}

/// Deliver an incoming UDP datagram to the appropriate socket.
///
/// Called by the net processing task on each received UDP packet.
pub fn deliver(src_ip: u32, dst_port: u16, src_port: u16, data: &[u8]) {
    for sock in table().iter_mut() {
        if sock.local_port == dst_port {
            if sock.recv_q.len() >= RECV_QUEUE_DEPTH { sock.recv_q.pop_front(); }
            sock.recv_q.push_back(Datagram {
                src_ip,
                src_port,
                data: Vec::from(data),
            });
            if let Some(waiter) = sock.waiter.take() {
                crate::task::wake_task(waiter);
            }
            return;
        }
    }
}

/// Non-blocking receive from a UDP socket.
///
/// Returns `Some((src_ip, src_port, bytes_written))` if a datagram was waiting,
/// or `None` if the queue is empty.
pub fn try_recv(fd: i64, buf: &mut [u8]) -> Option<(u32, u16, usize)> {
    for sock in table().iter_mut() {
        if sock.id == fd {
            if let Some(dg) = sock.recv_q.pop_front() {
                let n = dg.data.len().min(buf.len());
                buf[..n].copy_from_slice(&dg.data[..n]);
                return Some((dg.src_ip, dg.src_port, n));
            }
            return None;
        }
    }
    None
}

/// Blocking receive from a UDP socket.
///
/// Blocks the calling task until a datagram arrives.
pub fn recv_blocking(fd: i64, buf: &mut [u8]) -> Option<(u32, u16, usize)> {
    loop {
        {
            for sock in table().iter_mut() {
                if sock.id == fd {
                    if let Some(dg) = sock.recv_q.pop_front() {
                        let n = dg.data.len().min(buf.len());
                        buf[..n].copy_from_slice(&dg.data[..n]);
                        return Some((dg.src_ip, dg.src_port, n));
                    }
                    sock.waiter = Some(crate::task::current_task_id());
                    break;
                }
            }
        }
        crate::task::block_and_yield();
    }
}

/// Parse and handle an incoming UDP packet.
pub fn handle(src_ip: u32, dst_ip: u32, pkt: &[u8]) {
    let _ = dst_ip;
    if pkt.len() < UDP_HDR_LEN { return; }
    let src_port = u16::from_be_bytes([pkt[0], pkt[1]]);
    let dst_port = u16::from_be_bytes([pkt[2], pkt[3]]);
    let length   = u16::from_be_bytes([pkt[4], pkt[5]]) as usize;
    if length < UDP_HDR_LEN || length > pkt.len() { return; }
    deliver(src_ip, dst_port, src_port, &pkt[UDP_HDR_LEN..length]);
}

/// Close and remove a socket.
pub fn close(fd: i64) {
    let t = table();
    if let Some(pos) = t.iter().position(|s| s.id == fd) {
        t.swap_remove(pos);
    }
}
