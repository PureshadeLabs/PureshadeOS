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
    find_nth_device(vendor_id, device_id, 0)
}

/// Scan bus 0 for the `skip`+1'th device matching `vendor_id` / `device_id`
/// (i.e. `skip = 0` is the first match, `skip = 1` the second). Bus 0 is
/// scanned in ascending device-slot order, which is stable for a fixed QEMU
/// `-device` ordering — the root virtio-blk is instance 0, the persistent
/// store disk (attached after it) is instance 1.
///
/// Side effect: enables PCI bus mastering on the returned device so it can DMA.
pub fn find_nth_device(vendor_id: u16, device_id: u16, skip: usize) -> Option<PciDevice> {
    let mut seen = 0usize;
    for dev in 0u8..32 {
        let id = unsafe { cfg_read32(0, dev, 0x00) };
        if id == 0xFFFF_FFFF || id == 0x0000_0000 { continue; }

        let v = id as u16;
        let d = (id >> 16) as u16;
        if v != vendor_id || d != device_id { continue; }

        let bar0     = unsafe { cfg_read32(0, dev, 0x10) };
        // BAR0 must be an I/O space BAR (bit 0 = 1) for legacy VirtIO.
        if bar0 & 1 == 0 { continue; }

        if seen < skip {
            seen += 1;
            continue;
        }

        let irq_info = unsafe { cfg_read32(0, dev, 0x3C) };

        // Enable Bus Master Enable (bit 2) in the PCI Command register so the
        // device can issue DMA reads/writes to host RAM.
        let cmd = unsafe { cfg_read32(0, dev, 0x04) };
        unsafe { cfg_write32(0, dev, 0x04, cmd | (1 << 2)) };

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

// ── Device registry (userspace device-driver framework) ───────────────────────
//
// The kernel enumerates PCI devices it does NOT drive itself and records them
// in a registry.  lythd claims each device by name (SYS_DEV_CLAIM), receives an
// unforgeable Device capability, and hands it to the intended userspace driver.
// The kernel never touches a registered device's registers, IRQ, or DMA — a
// driver process does, authorized solely by its Device cap.
//
// virtio-blk (root disk + persistent store) is kernel-owned and is deliberately
// NOT registered here, so it can never be claimed/seized from userspace.

use core::cell::UnsafeCell;

/// Maximum PCI devices the registry tracks.
pub const MAX_REGISTERED_DEVICES: usize = 8;

/// One Base Address Register of a registered device.
#[derive(Clone, Copy)]
pub struct DeviceBar {
    /// Physical base address (0 if the BAR is unimplemented).
    pub base:   u64,
    /// Region size in bytes (0 if unimplemented / the high half of a 64-bit BAR).
    pub size:   u64,
    /// True for a memory BAR (mappable via SYS_DEV_MMIO_MAP); false for I/O-port.
    pub is_mem: bool,
}

impl DeviceBar {
    const EMPTY: Self = Self { base: 0, size: 0, is_mem: false };
}

/// A PCI device recorded in the registry, claimable by name.
#[derive(Clone, Copy)]
pub struct RegisteredDevice {
    pub name:     &'static str,
    pub bus:      u8,
    pub dev:      u8,
    pub vendor:   u16,
    pub device:   u16,
    pub bars:     [DeviceBar; 6],
    /// PCI interrupt line (GSI) from config space.
    pub irq_line: u8,
    /// True once claimed via SYS_DEV_CLAIM (a device is claimable at most once).
    pub claimed:  bool,
}

struct Registry {
    devs: [Option<RegisteredDevice>; MAX_REGISTERED_DEVICES],
    len:  usize,
}

struct RegCell(UnsafeCell<Registry>);
// SAFETY: single-threaded kernel; registry is built at boot then read/claimed
// under the same single-core discipline as the rest of the kernel.
unsafe impl Sync for RegCell {}

static REGISTRY: RegCell = RegCell(UnsafeCell::new(Registry {
    devs: [const { None }; MAX_REGISTERED_DEVICES],
    len:  0,
}));

#[inline]
fn registry() -> &'static mut Registry {
    unsafe { &mut *REGISTRY.0.get() }
}

/// Devices the kernel does not drive itself, registered for userspace claiming.
/// Each entry: (vendor, device, stable name handed to lythd).
const REGISTERED_ALLOWLIST: &[(u16, u16, &str)] = &[
    // virtio-net (transitional virtio-net-pci): owned by the userspace `netd`.
    (0x1AF4, 0x1000, "virtio-net"),
];

/// Size one BAR by the standard write-all-ones / read-back-mask probe, then
/// restore the original value.  Returns the decoded `DeviceBar` and whether it
/// consumed a second BAR slot (64-bit memory BAR).
unsafe fn probe_bar(bus: u8, dev: u8, bar_idx: u8) -> (DeviceBar, bool) {
    let off = 0x10 + bar_idx * 4;
    let orig = unsafe { cfg_read32(bus, dev, off) };

    if orig & 1 == 1 {
        // I/O-space BAR (16-bit port window).
        unsafe { cfg_write32(bus, dev, off, 0xFFFF_FFFF) };
        let mask = unsafe { cfg_read32(bus, dev, off) };
        unsafe { cfg_write32(bus, dev, off, orig) };
        let size = (!(mask & !0x3) & 0xFFFF).wrapping_add(1) as u64;
        let base = (orig & !0x3) as u64;
        let size = if base == 0 { 0 } else { size };
        (DeviceBar { base, size, is_mem: false }, false)
    } else {
        let is_64  = (orig >> 1) & 0x3 == 0x2;
        let base_lo = orig & !0xF;
        unsafe { cfg_write32(bus, dev, off, 0xFFFF_FFFF) };
        let mask_lo = unsafe { cfg_read32(bus, dev, off) };
        unsafe { cfg_write32(bus, dev, off, orig) };

        if is_64 {
            let orig_hi = unsafe { cfg_read32(bus, dev, off + 4) };
            unsafe { cfg_write32(bus, dev, off + 4, 0xFFFF_FFFF) };
            let mask_hi = unsafe { cfg_read32(bus, dev, off + 4) };
            unsafe { cfg_write32(bus, dev, off + 4, orig_hi) };
            let base = ((orig_hi as u64) << 32) | base_lo as u64;
            let mask = ((mask_hi as u64) << 32) | (mask_lo & !0xF) as u64;
            let size = if mask == 0 { 0 } else { (!mask).wrapping_add(1) };
            (DeviceBar { base, size, is_mem: true }, true)
        } else {
            let masked = mask_lo & !0xF;
            let size = if masked == 0 { 0 } else { (!masked).wrapping_add(1) as u64 };
            (DeviceBar { base: base_lo as u64, size, is_mem: true }, false)
        }
    }
}

/// Enumerate bus 0 and register every allowlisted device the kernel does not
/// drive.  Enables memory + I/O + bus-master on each so a userspace driver can
/// access MMIO and the device can DMA.  Call once at boot, after PCI/IOAPIC init.
pub fn init_device_registry() {
    let reg = registry();
    reg.len = 0;

    for dev in 0u8..32 {
        if reg.len >= MAX_REGISTERED_DEVICES { break; }
        let id = unsafe { cfg_read32(0, dev, 0x00) };
        if id == 0xFFFF_FFFF || id == 0x0000_0000 { continue; }
        let vendor = id as u16;
        let device = (id >> 16) as u16;

        let Some(&(_, _, name)) = REGISTERED_ALLOWLIST
            .iter()
            .find(|(v, d, _)| *v == vendor && *d == device)
        else { continue; };

        // Enable memory-space + I/O-space + bus-master (bits 0,1,2).
        let cmd = unsafe { cfg_read32(0, dev, 0x04) };
        unsafe { cfg_write32(0, dev, 0x04, cmd | 0x7) };

        // Probe all six BARs (64-bit BARs consume two slots).
        let mut bars = [DeviceBar::EMPTY; 6];
        let mut i = 0u8;
        while i < 6 {
            let (bar, took_two) = unsafe { probe_bar(0, dev, i) };
            bars[i as usize] = bar;
            i += if took_two { 2 } else { 1 };
        }

        let irq_line = (unsafe { cfg_read32(0, dev, 0x3C) } & 0xFF) as u8;

        reg.devs[reg.len] = Some(RegisteredDevice {
            name, bus: 0, dev, vendor, device, bars, irq_line, claimed: false,
        });
        reg.len += 1;

        crate::kprintln!(
            "[pci-reg] '{}' {:04x}:{:04x} slot {} irq {} bar0={:#x}/{:#x} bar4={:#x}/{:#x}",
            name, vendor, device, dev, irq_line,
            bars[0].base, bars[0].size, bars[4].base, bars[4].size,
        );
    }
}

/// Number of registered (claimable) devices.
pub fn registry_len() -> usize { registry().len }

/// Immutable view of registered device `idx`.
pub fn registry_get(idx: usize) -> Option<&'static RegisteredDevice> {
    registry().devs.get(idx).and_then(|d| d.as_ref())
}

/// Find a registered device by name; returns its registry index.
pub fn find_registered(name: &str) -> Option<usize> {
    let reg = registry();
    (0..reg.len).find(|&i| {
        reg.devs[i].as_ref().map_or(false, |d| d.name == name)
    })
}

/// Mark device `idx` claimed.  Returns false if already claimed or out of range.
pub fn mark_claimed(idx: usize) -> bool {
    let reg = registry();
    match reg.devs.get_mut(idx).and_then(|d| d.as_mut()) {
        Some(d) if !d.claimed => { d.claimed = true; true }
        _ => false,
    }
}

/// Read one 32-bit dword from registered device `idx`'s PCI config space.
pub fn registry_cfg_read(idx: usize, offset: u8) -> Option<u32> {
    let d = registry_get(idx)?;
    Some(unsafe { cfg_read32(d.bus, d.dev, offset) })
}
