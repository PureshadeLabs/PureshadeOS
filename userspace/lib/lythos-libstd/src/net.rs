//! Network stub for Lythos.
//!
//! Lythos uses capability-gated IPC rather than BSD sockets; TCP/IP is out of
//! scope until a network-stack task exists.  This module provides the type
//! skeletons so that code which mentions `std::net` compiles.

use crate::io::{Error, ErrorKind, Read, Write, Result};
use _alloc::vec::Vec;
use _alloc::string::String;

fn unsupported<T>() -> Result<T> {
    Err(Error::new(ErrorKind::Unsupported, "network not implemented"))
}

// ── Addr types ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Ipv4Addr([u8; 4]);

impl Ipv4Addr {
    pub const LOCALHOST: Self = Ipv4Addr([127, 0, 0, 1]);
    pub const UNSPECIFIED: Self = Ipv4Addr([0, 0, 0, 0]);
    pub const BROADCAST: Self = Ipv4Addr([255, 255, 255, 255]);

    pub fn new(a: u8, b: u8, c: u8, d: u8) -> Self { Ipv4Addr([a, b, c, d]) }
    pub fn octets(&self) -> [u8; 4] { self.0 }
}

impl core::fmt::Display for Ipv4Addr {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let [a, b, c, d] = self.0;
        write!(f, "{}.{}.{}.{}", a, b, c, d)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Ipv6Addr([u16; 8]);

impl Ipv6Addr {
    pub const LOCALHOST: Self = Ipv6Addr([0, 0, 0, 0, 0, 0, 0, 1]);
    pub const UNSPECIFIED: Self = Ipv6Addr([0; 8]);

    pub fn new(a: u16, b: u16, c: u16, d: u16, e: u16, g: u16, h: u16, i: u16) -> Self {
        Ipv6Addr([a, b, c, d, e, g, h, i])
    }
    pub fn segments(&self) -> [u16; 8] { self.0 }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IpAddr {
    V4(Ipv4Addr),
    V6(Ipv6Addr),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SocketAddrV4 { ip: Ipv4Addr, port: u16 }
impl SocketAddrV4 {
    pub fn new(ip: Ipv4Addr, port: u16) -> Self { SocketAddrV4 { ip, port } }
    pub fn ip(&self) -> &Ipv4Addr { &self.ip }
    pub fn port(&self) -> u16 { self.port }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SocketAddrV6 { ip: Ipv6Addr, port: u16 }
impl SocketAddrV6 {
    pub fn new(ip: Ipv6Addr, port: u16, _flowinfo: u32, _scope_id: u32) -> Self {
        SocketAddrV6 { ip, port }
    }
    pub fn ip(&self) -> &Ipv6Addr { &self.ip }
    pub fn port(&self) -> u16 { self.port }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SocketAddr {
    V4(SocketAddrV4),
    V6(SocketAddrV6),
}

impl SocketAddr {
    pub fn port(&self) -> u16 {
        match self { SocketAddr::V4(a) => a.port, SocketAddr::V6(a) => a.port }
    }
    pub fn ip(&self) -> IpAddr {
        match self {
            SocketAddr::V4(a) => IpAddr::V4(a.ip),
            SocketAddr::V6(a) => IpAddr::V6(a.ip),
        }
    }
}

// ── TCP stubs ─────────────────────────────────────────────────────────────────

pub struct TcpStream(());
pub struct TcpListener(());

impl TcpStream {
    pub fn connect(_addr: SocketAddr) -> Result<Self> { unsupported() }
    pub fn peer_addr(&self) -> Result<SocketAddr> { unsupported() }
    pub fn local_addr(&self) -> Result<SocketAddr> { unsupported() }
    pub fn shutdown(&self, _how: Shutdown) -> Result<()> { unsupported() }
}

impl Read for TcpStream {
    fn read(&mut self, _buf: &mut [u8]) -> Result<usize> { unsupported() }
}

impl Write for TcpStream {
    fn write(&mut self, _buf: &[u8]) -> Result<usize> { unsupported() }
    fn flush(&mut self) -> Result<()> { unsupported() }
}

impl TcpListener {
    pub fn bind(_addr: SocketAddr) -> Result<Self> { unsupported() }
    pub fn accept(&self) -> Result<(TcpStream, SocketAddr)> { unsupported() }
    pub fn local_addr(&self) -> Result<SocketAddr> { unsupported() }
}

// ── UDP stubs ─────────────────────────────────────────────────────────────────

pub struct UdpSocket(());

impl UdpSocket {
    pub fn bind(_addr: SocketAddr) -> Result<Self> { unsupported() }
    pub fn send_to(&self, _buf: &[u8], _addr: SocketAddr) -> Result<usize> { unsupported() }
    pub fn recv_from(&self, _buf: &mut [u8]) -> Result<(usize, SocketAddr)> { unsupported() }
    pub fn local_addr(&self) -> Result<SocketAddr> { unsupported() }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shutdown { Read, Write, Both }
