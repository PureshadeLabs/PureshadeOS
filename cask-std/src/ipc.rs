//! Inter-process communication — IPC endpoints and typed channels.
//!
//! cask IPC uses fixed 64-byte message slots in a per-endpoint ring buffer.
//! Both `send` and `recv` block when the ring is full/empty respectively.

use crate::io;

/// Size of a single IPC message frame, in bytes.
pub const MSG_SIZE: usize = 64;

// ── Endpoint ──────────────────────────────────────────────────────────────────

/// A raw IPC endpoint backed by a cask capability.
///
/// The underlying ring buffer holds up to 63 message slots.
///
/// # Blocking behaviour
/// - `send` blocks if the ring is full.
/// - `recv` blocks if the ring is empty.
pub struct Endpoint {
    cap: u64,
}

impl Endpoint {
    /// Allocate a new IPC endpoint. The caller holds all rights to it.
    pub fn create() -> io::Result<Self> {
        crate::sys_ipc_create()
            .map(|cap| Endpoint { cap })
            .map_err(io::Error::from_kernel)
    }

    /// Wrap a raw capability handle obtained from the kernel or another task.
    pub fn from_raw(cap: u64) -> Self { Endpoint { cap } }

    /// Return the underlying capability handle.
    pub fn as_raw(&self) -> u64 { self.cap }

    /// Send bytes to this endpoint. Blocks if the ring is full.
    ///
    /// Only the first `MSG_SIZE` (64) bytes of `msg` are sent.
    pub fn send(&self, msg: &[u8]) -> io::Result<()> {
        crate::sys_ipc_send(self.cap, msg).map_err(io::Error::from_kernel)
    }

    /// Receive bytes from this endpoint into `buf`. Blocks if the ring is empty.
    ///
    /// Returns the number of bytes written. Up to `MSG_SIZE` (64) bytes are
    /// written; `buf` shorter than that will be silently truncated on read.
    pub fn recv(&self, buf: &mut [u8]) -> io::Result<usize> {
        crate::sys_ipc_recv(self.cap, buf).map_err(io::Error::from_kernel)
    }

    /// Send a fixed-size 64-byte frame.
    pub fn send_frame(&self, frame: &[u8; MSG_SIZE]) -> io::Result<()> {
        self.send(frame)
    }

    /// Receive a fixed-size 64-byte frame, blocking until one arrives.
    pub fn recv_frame(&self) -> io::Result<[u8; MSG_SIZE]> {
        let mut buf = [0u8; MSG_SIZE];
        self.recv(&mut buf)?;
        Ok(buf)
    }

    /// Send bytes **and** transfer a capability handle in one atomic operation.
    ///
    /// `cap` is moved out of the caller's capability table and delivered to the
    /// receiver alongside the message.  Blocks if the ring is full.
    pub fn send_with_cap(&self, msg: &[u8], cap: u64) -> io::Result<()> {
        crate::sys_ipc_send_cap(self.cap, msg, cap).map_err(io::Error::from_kernel)
    }

    /// Receive bytes and accept any in-flight capability, blocking until a
    /// message arrives.
    ///
    /// Returns `(bytes_written, Some(handle))` when a capability was attached,
    /// or `(bytes_written, None)` when the message carried no capability.
    pub fn recv_with_cap(&self, buf: &mut [u8]) -> io::Result<(usize, Option<u64>)> {
        crate::sys_ipc_recv_cap(self.cap, buf).map_err(io::Error::from_kernel)
    }

    /// Receive a fixed 64-byte frame and accept any in-flight capability.
    pub fn recv_frame_with_cap(&self) -> io::Result<([u8; MSG_SIZE], Option<u64>)> {
        let mut buf = [0u8; MSG_SIZE];
        let (_, cap) = self.recv_with_cap(&mut buf)?;
        Ok((buf, cap))
    }
}

// ── Message ───────────────────────────────────────────────────────────────────

/// A type that can be packed into and unpacked from a 64-byte IPC frame.
///
/// Implement this for your message types to use [`Channel<T>`].
///
/// # Example
/// ```rust,ignore
/// struct Ping { seq: u32 }
///
/// impl Message for Ping {
///     fn encode(&self, buf: &mut [u8; 64]) {
///         buf[..4].copy_from_slice(&self.seq.to_le_bytes());
///     }
///     fn decode(buf: &[u8; 64]) -> Option<Self> {
///         Some(Ping { seq: u32::from_le_bytes(buf[..4].try_into().ok()?) })
///     }
/// }
/// ```
pub trait Message: Sized {
    /// Serialise `self` into a 64-byte frame.
    fn encode(&self, buf: &mut [u8; MSG_SIZE]);
    /// Deserialise from a 64-byte frame. Returns `None` if the frame is invalid.
    fn decode(buf: &[u8; MSG_SIZE]) -> Option<Self>;
}

// ── Channel<T> ────────────────────────────────────────────────────────────────

/// A typed IPC channel that sends and receives values implementing [`Message`].
pub struct Channel<T: Message> {
    ep:       Endpoint,
    _phantom: core::marker::PhantomData<T>,
}

impl<T: Message> Channel<T> {
    /// Create a new channel (allocates a new IPC endpoint).
    pub fn create() -> io::Result<Self> {
        Endpoint::create().map(|ep| Channel { ep, _phantom: core::marker::PhantomData })
    }

    /// Wrap a raw capability handle as a typed channel.
    pub fn from_raw(cap: u64) -> Self {
        Channel { ep: Endpoint::from_raw(cap), _phantom: core::marker::PhantomData }
    }

    /// Return the underlying raw capability handle.
    pub fn as_raw(&self) -> u64 { self.ep.as_raw() }

    /// Return a reference to the underlying raw [`Endpoint`].
    pub fn endpoint(&self) -> &Endpoint { &self.ep }

    /// Encode and send `msg`. Blocks if the ring is full.
    pub fn send(&self, msg: &T) -> io::Result<()> {
        let mut buf = [0u8; MSG_SIZE];
        msg.encode(&mut buf);
        self.ep.send_frame(&buf)
    }

    /// Receive and decode one message. Blocks if the ring is empty.
    ///
    /// Returns `io::Error::INVALID_DATA` if the frame fails to decode.
    pub fn recv(&self) -> io::Result<T> {
        let buf = self.ep.recv_frame()?;
        T::decode(&buf).ok_or(io::Error::INVALID_DATA)
    }
}
