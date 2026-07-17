//! Modern virtio-pci capability discovery.
//!
//! A modern virtio device advertises the location of its configuration
//! structures through vendor-specific (0x09) PCI capabilities. Each names a BAR
//! index plus a byte offset/length within it. We walk the capability list via
//! `sys_dev_cfg_read` (the framework's gated config-space read) — no port-I/O
//! authority required — and record where the common-config, notify, ISR, and
//! device-config structures live.

use lythos_rt::sys_dev_cfg_read;

/// virtio cfg_type: common configuration structure.
pub const CAP_COMMON: u8 = 1;
/// virtio cfg_type: notification structure.
pub const CAP_NOTIFY: u8 = 2;
/// virtio cfg_type: ISR status structure.
pub const CAP_ISR: u8 = 3;
/// virtio cfg_type: device-specific configuration structure.
pub const CAP_DEVICE: u8 = 4;

/// Vendor-specific PCI capability id (virtio uses these).
const PCI_CAP_ID_VNDR: u8 = 0x09;

/// Location of one virtio configuration structure within a BAR.
#[derive(Clone, Copy, Default)]
pub struct CapLoc {
    pub present: bool,
    pub bar:     u8,
    pub offset:  u32,
    pub length:  u32,
}

/// Discovered modern-virtio layout.
#[derive(Clone, Copy, Default)]
pub struct VirtioLayout {
    pub common:      CapLoc,
    pub notify:      CapLoc,
    pub isr:         CapLoc,
    pub device:      CapLoc,
    /// `queue_notify_off` multiplier for computing per-queue notify addresses.
    pub notify_mult: u32,
}

impl VirtioLayout {
    /// True if the minimum structures for modern operation were found.
    pub fn is_modern(&self) -> bool {
        self.common.present && self.notify.present && self.isr.present
    }
}

#[inline]
fn cfg32(dev_cap: u64, off: u32) -> u32 {
    sys_dev_cfg_read(dev_cap, off).unwrap_or(0)
}

/// Walk the device's PCI capability list and locate its virtio structures.
pub fn discover(dev_cap: u64) -> VirtioLayout {
    let mut lay = VirtioLayout::default();

    // Status register (bits 16..32 of dword 0x04); bit 4 => capability list.
    let status = (cfg32(dev_cap, 0x04) >> 16) as u16;
    if status & (1 << 4) == 0 { return lay; }

    // First capability pointer (low byte of dword 0x34), dword-aligned.
    let mut ptr = cfg32(dev_cap, 0x34) & 0xFF;
    let mut guard = 0;
    while ptr != 0 && ptr < 252 && guard < 48 {
        guard += 1;
        let d0   = cfg32(dev_cap, ptr & !3);
        let vndr = (d0 & 0xFF) as u8;
        let next = (d0 >> 8) & 0xFF;

        if vndr == PCI_CAP_ID_VNDR {
            // virtio vendor cap:
            //   +0 cap_vndr(u8) +1 cap_next(u8) +2 cap_len(u8) +3 cfg_type(u8)
            //   +4 bar(u8) +5 pad[3] +8 offset(u32) +12 length(u32)
            //   +16 notify_off_multiplier(u32)   [notify cap only]
            let cfg_type = ((d0 >> 24) & 0xFF) as u8;
            let bar      = (cfg32(dev_cap, ptr + 4) & 0xFF) as u8;
            let offset   = cfg32(dev_cap, ptr + 8);
            let length   = cfg32(dev_cap, ptr + 12);
            let loc = CapLoc { present: true, bar, offset, length };
            match cfg_type {
                CAP_COMMON => lay.common = loc,
                CAP_NOTIFY => {
                    lay.notify = loc;
                    lay.notify_mult = cfg32(dev_cap, ptr + 16);
                }
                CAP_ISR    => lay.isr = loc,
                CAP_DEVICE => lay.device = loc,
                _ => {}
            }
        }
        ptr = next;
    }
    lay
}
