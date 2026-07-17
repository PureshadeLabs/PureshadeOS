//! Split-virtqueue ring layout + producer/consumer helpers.
//!
//! The split virtqueue (descriptor table + available ring + used ring) is
//! identical for legacy and modern virtio — this mirrors the ring math in the
//! in-kernel virtio-blk driver. Only the transport (how the ring addresses are
//! handed to the device, and how the device is notified) differs; that lives in
//! the driver. A [`SplitQueue`] is laid over one contiguous DMA buffer.

use core::sync::atomic::{fence, Ordering};

use crate::dma::DmaBuf;

/// Descriptor chains to the `next` field.
pub const VIRTQ_DESC_F_NEXT: u16 = 1;
/// Device writes into this buffer (device → driver).
pub const VIRTQ_DESC_F_WRITE: u16 = 2;

/// Bytes needed for a split virtqueue of `qsz` entries (desc + avail + used,
/// with the used ring 4-byte aligned).
pub fn bytes(qsz: u16) -> u64 {
    let q = qsz as u64;
    let avail_off = 16 * q;
    let used_off = (avail_off + 6 + 2 * q + 3) & !3;
    used_off + 6 + 8 * q
}

/// A split virtqueue laid over a DMA buffer.
pub struct SplitQueue {
    pub qsz:        u16,
    desc:           u64, // virt: descriptor table
    avail:          u64, // virt: available ring
    used:           u64, // virt: used ring
    pub desc_phys:  u64,
    pub avail_phys: u64,
    pub used_phys:  u64,
    pub avail_idx:  u16, // next available ring slot to produce
    pub last_used:  u16, // last used ring index consumed
}

impl SplitQueue {
    /// Lay a queue of `qsz` entries over `buf` (which must be ≥ `bytes(qsz)`).
    pub fn new(buf: DmaBuf, qsz: u16) -> Self {
        let q = qsz as u64;
        let avail_off = 16 * q;
        let used_off = (avail_off + 6 + 2 * q + 3) & !3;
        Self {
            qsz,
            desc:       buf.virt,
            avail:      buf.virt + avail_off,
            used:       buf.virt + used_off,
            desc_phys:  buf.phys,
            avail_phys: buf.phys + avail_off,
            used_phys:  buf.phys + used_off,
            avail_idx:  0,
            last_used:  0,
        }
    }

    /// Write descriptor `i` (physical `addr`, `len` bytes, `flags`, `next`).
    pub fn set_desc(&self, i: u16, addr: u64, len: u32, flags: u16, next: u16) {
        let base = self.desc + i as u64 * 16;
        unsafe {
            (base as *mut u64).write_volatile(addr);
            ((base + 8) as *mut u32).write_volatile(len);
            ((base + 12) as *mut u16).write_volatile(flags);
            ((base + 14) as *mut u16).write_volatile(next);
        }
    }

    /// Publish descriptor-chain head `head` into the available ring and bump
    /// `avail.idx` (with a release fence so the device sees the descriptors).
    pub fn publish(&mut self, head: u16) {
        let slot = self.avail_idx % self.qsz;
        unsafe {
            // avail.ring[slot] = head  (ring starts at avail + 4)
            ((self.avail + 4 + slot as u64 * 2) as *mut u16).write_volatile(head);
        }
        self.avail_idx = self.avail_idx.wrapping_add(1);
        fence(Ordering::Release);
        unsafe {
            // avail.idx (avail + 2)
            ((self.avail + 2) as *mut u16).write_volatile(self.avail_idx);
        }
    }

    /// Current device-published used index.
    pub fn used_idx(&self) -> u16 {
        fence(Ordering::Acquire);
        unsafe { ((self.used + 2) as *const u16).read_volatile() }
    }

    /// Pop one completed used-ring entry `(desc_id, written_len)`, or `None`.
    pub fn take_used(&mut self) -> Option<(u32, u32)> {
        if self.last_used == self.used_idx() { return None; }
        let slot = self.last_used % self.qsz;
        // used.ring starts at used + 4; each element is {id:u32, len:u32}.
        let elem = self.used + 4 + slot as u64 * 8;
        let id  = unsafe { (elem as *const u32).read_volatile() };
        let len = unsafe { ((elem + 4) as *const u32).read_volatile() };
        self.last_used = self.last_used.wrapping_add(1);
        Some((id, len))
    }
}
