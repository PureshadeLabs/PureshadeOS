//! PCI configuration space scanner — mechanism 1 (I/O ports 0xCF8 / 0xCFC).
//!
//! Scans bus 0 only.  Multi-bus enumeration (bridges) deferred until needed.

const CONFIG_ADDRESS: u16 = 0xCF8;
const CONFIG_DATA:    u16 = 0xCFC;

// ── Summary type ──────────────────────────────────────────────────────────────

/// A PCI device located by `find_device`.
pub struct PciDevice {
    pub bus:      u8,
    pub dev:      u8,
    pub vendor:   u16,
    pub device:   u16,
    /// I/O port base from BAR0 (BAR0 & !0x3).  Only valid for I/O-space BARs.
    pub io_bar0:  u16,
    pub irq_line: u8,
}

// ── Config-space address encoding ────────────────────────────────────────────

fn config_addr(bus: u8, dev: u8, func: u8, off: u8) -> u32 {
    (1u32 << 31)
        | ((bus  as u32) << 16)
        | ((dev  as u32) << 11)
        | ((func as u32) <<  8)
        | ((off & 0xFC) as u32)
}

// ── I/O port helpers (32-bit) ────────────────────────────────────────────────

#[inline]
unsafe fn outl(port: u16, val: u32) {
    unsafe {
        core::arch::asm!(
            "out dx, eax",
            in("dx") port, in("eax") val,
            options(nomem, nostack, preserves_flags),
        );
    }
}

#[inline]
unsafe fn inl(port: u16) -> u32 {
    let val: u32;
    unsafe {
        core::arch::asm!(
            "in eax, dx",
            in("dx") port, out("eax") val,
            options(nomem, nostack, preserves_flags),
        );
    }
    val
}

// ── Config-space read / write ────────────────────────────────────────────────

unsafe fn cfg_read32(bus: u8, dev: u8, off: u8) -> u32 {
    let addr = config_addr(bus, dev, 0, off);
    unsafe {
        outl(CONFIG_ADDRESS, addr);
        inl(CONFIG_DATA)
    }
}

unsafe fn cfg_write32(bus: u8, dev: u8, off: u8, val: u32) {
    let addr = config_addr(bus, dev, 0, off);
    unsafe {
        outl(CONFIG_ADDRESS, addr);
        outl(CONFIG_DATA, val);
    }
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Scan bus 0 for a device and return its (bus, dev) slot.
/// Does not require a specific BAR type — use `read_bar32` to inspect BARs.
pub fn find(vendor_id: u16, device_id: u16) -> Option<(u8, u8)> {
    for dev in 0u8..32 {
        let id = unsafe { cfg_read32(0, dev, 0x00) };
        if id == 0xFFFF_FFFF || id == 0 { continue; }
        if id as u16 == vendor_id && (id >> 16) as u16 == device_id {
            return Some((0, dev));
        }
    }
    None
}

/// Read raw 32-bit BAR value.  `bar_idx` 0..5 = BAR0..BAR5.
pub fn read_bar32(bus: u8, dev: u8, bar_idx: u8) -> u32 {
    unsafe { cfg_read32(bus, dev, 0x10 + bar_idx * 4) }
}

/// Enable I/O-space + Memory-space access for a device (PCI command bits 0+1).
pub fn enable_io_mem(bus: u8, dev: u8) {
    let cmd = unsafe { cfg_read32(bus, dev, 0x04) };
    unsafe { cfg_write32(bus, dev, 0x04, cmd | 0x3) };
}

/// Scan bus 0 for the first device matching `vendor_id` / `device_id`.
///
/// Side effect: enables PCI bus mastering on the found device so the device
/// can perform DMA to/from host memory.
pub fn find_device(vendor_id: u16, device_id: u16) -> Option<PciDevice> {
    for dev in 0u8..32 {
        let id = unsafe { cfg_read32(0, dev, 0x00) };
        if id == 0xFFFF_FFFF || id == 0x0000_0000 { continue; }

        let v = id as u16;
        let d = (id >> 16) as u16;
        if v != vendor_id || d != device_id { continue; }

        let bar0     = unsafe { cfg_read32(0, dev, 0x10) };
        let irq_info = unsafe { cfg_read32(0, dev, 0x3C) };

        // Enable Bus Master Enable (bit 2) in the PCI Command register so the
        // device can issue DMA reads/writes to host RAM.
        let cmd = unsafe { cfg_read32(0, dev, 0x04) };
        unsafe { cfg_write32(0, dev, 0x04, cmd | (1 << 2)) };

        // BAR0 must be an I/O space BAR (bit 0 = 1) for legacy VirtIO.
        if bar0 & 1 == 0 { continue; }

        return Some(PciDevice {
            bus: 0,
            dev,
            vendor:   v,
            device:   d,
            io_bar0:  (bar0 & !0x3) as u16,
            irq_line: (irq_info & 0xFF) as u8,
        });
    }
    None
}
