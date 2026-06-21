//! VirtIO legacy block device driver (virtio-blk, PCI transport).
//!
//! Implements synchronous interrupt-driven single-sector read/write using a
//! 3-descriptor chain.  The driver submits a request, halts the CPU, and
//! wakes on the virtio-blk PCI IRQ routed through the I/O APIC.
//!
//! ## Hardware interface
//!
//! VirtIO legacy (spec 0.9.5) uses I/O-space BAR0 for all control registers.
//! BAR0 is discovered via the PCI scanner (`pci::find_device`).
//!
//! ## Virtqueue layout (Q = queue size, page-aligned)
//!
//! ```text
//! offset 0          : descriptor table — Q × 16 bytes
//! offset 16Q        : available ring   — 4 + 2Q bytes
//! offset ALIGN(…,4K): used ring        — 4 + 8Q bytes
//! ```
//!
//! The driver supports up to `QUEUE_SIZE_MAX = 1024` entries; QEMU 10.x
//! defaults to 256.  Up to 8 contiguous 4 KiB pages are allocated.
//!
//! ## DMA buffers (identity-mapped, phys == virt for RAM < 1 GiB)
//!
//! * `hdr_phys`  — one frame: 16-byte `VirtioBlkReq` + 1-byte status at +16.
//! * `dat_phys`  — one frame: 512-byte sector data.
//!
//! ## QEMU invocation
//!
//! Pass a disk image to QEMU:
//! ```
//! -drive file=disk.img,format=raw,if=none,id=hd0 \
//! -device virtio-blk-pci,drive=hd0
//! ```

use core::arch::global_asm;
use core::sync::atomic::{self, Ordering};

// ── IRQ vector ────────────────────────────────────────────────────────────────

/// IDT vector for the virtio-blk PCI interrupt.  34 is free (32=timer, 33=TLB).
pub const VECTOR_VIRTIO_BLK: u8 = 34;

// ── IRQ stub ──────────────────────────────────────────────────────────────────
//
// Saves caller-saved registers, calls virtio_blk_irq_handler (reads ISR_STATUS
// to satisfy the device and sends APIC EOI), then restores and iretq.
// The handler does nothing else — the submit() poll loop reads the status byte
// directly and detects completion on the next iteration.

global_asm!(r#"
.section .text
.global virtio_blk_isr_stub
.type   virtio_blk_isr_stub, @function
virtio_blk_isr_stub:
    pushq  %rax
    pushq  %rcx
    pushq  %rdx
    pushq  %rsi
    pushq  %rdi
    pushq  %r8
    pushq  %r9
    pushq  %r10
    pushq  %r11
    call   virtio_blk_irq_handler
    popq   %r11
    popq   %r10
    popq   %r9
    popq   %r8
    popq   %rdi
    popq   %rsi
    popq   %rdx
    popq   %rcx
    popq   %rax
    iretq
"#, options(att_syntax));

unsafe extern "C" { fn virtio_blk_isr_stub(); }

/// Called from `virtio_blk_isr_stub` on every virtio-blk PCI interrupt.
///
/// Reads `ISR_STATUS` (clears the interrupt at the device), then sends EOI
/// to the Local APIC.  The `submit()` polling loop checks the status byte
/// independently and acts on completion, so no state needs to be set here.
#[unsafe(no_mangle)]
pub extern "C" fn virtio_blk_irq_handler() {
    if let Some(dev) = dev_ref() {
        let _ = unsafe { inb(dev.io_base + REG_ISR_STATUS) };
    }
    crate::apic::eoi();
}

// ── PCI IDs ───────────────────────────────────────────────────────────────────

const VIRTIO_VENDOR:    u16 = 0x1AF4;
const VIRTIO_BLK_DEV:  u16 = 0x1001;

// ── VirtIO legacy I/O register offsets (relative to BAR0 I/O base) ───────────

const REG_DEVICE_FEATURES: u16 = 0x00; // [R]   device feature bits
const REG_GUEST_FEATURES:  u16 = 0x04; // [W]   driver feature bits
const REG_QUEUE_PFN:       u16 = 0x08; // [W]   virtqueue page frame number
const REG_QUEUE_NUM:       u16 = 0x0C; // [R]   max virtqueue size
const REG_QUEUE_SEL:       u16 = 0x0E; // [W]   select virtqueue
const REG_QUEUE_NOTIFY:    u16 = 0x10; // [W]   kick device (queue index)
const REG_DEVICE_STATUS:   u16 = 0x12; // [R/W] device status byte
const REG_ISR_STATUS:      u16 = 0x13; // [R]   ISR status; clears on read
const REG_BLK_CAPACITY_LO: u16 = 0x14; // [R]   low 32 bits of sector count
const REG_BLK_CAPACITY_HI: u16 = 0x18; // [R]   high 32 bits of sector count

// ── Device status flags ───────────────────────────────────────────────────────

const STATUS_ACKNOWLEDGE: u8 = 1;
const STATUS_DRIVER:      u8 = 2;
const STATUS_DRIVER_OK:   u8 = 4;

// ── Virtqueue descriptor flags ────────────────────────────────────────────────

const VIRTQ_DESC_F_NEXT:  u16 = 1; // descriptor is chained; `.next` is valid
const VIRTQ_DESC_F_WRITE: u16 = 2; // device writes into this buffer (device→guest)

// ── Block request type codes ──────────────────────────────────────────────────

const VIRTIO_BLK_T_IN:  u32 = 0; // read  (device writes sector data into guest buffer)
const VIRTIO_BLK_T_OUT: u32 = 1; // write (guest writes sector data into device)

// ── Sizing ────────────────────────────────────────────────────────────────────

/// Maximum virtqueue depth we support.  QEMU 10.x default is 256; cap at 1024.
const QUEUE_SIZE_MAX: usize = 1024;

/// Pages needed for the virtqueue when Q = QUEUE_SIZE_MAX = 1024.
///
/// desc table:  16 × 1024 = 16384 bytes  (4 pages)
/// avail ring:   4 + 2 × 1024 = 2052 bytes  (fits in page 4, ends at offset 18436)
/// used ring:   starts at next 4 KiB boundary (offset 20480 = 5 pages)
///              4 + 8 × 1024 = 8196 bytes  (ends at offset 28676)
/// → 8 pages (32768 bytes) covers any Q ≤ 1024
const QUEUE_PAGES: usize = 8;

/// Size of a VirtIO block sector in bytes.
pub const SECTOR_SIZE: usize = 512;

// ── Port I/O helpers ──────────────────────────────────────────────────────────

#[inline]
unsafe fn outb(port: u16, val: u8) {
    unsafe {
        core::arch::asm!(
            "out dx, al",
            in("dx") port, in("al") val,
            options(nomem, nostack, preserves_flags),
        );
    }
}

#[inline]
unsafe fn outw(port: u16, val: u16) {
    unsafe {
        core::arch::asm!(
            "out dx, ax",
            in("dx") port, in("ax") val,
            options(nomem, nostack, preserves_flags),
        );
    }
}

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
unsafe fn inb(port: u16) -> u8 {
    let val: u8;
    unsafe {
        core::arch::asm!(
            "in al, dx",
            in("dx") port, out("al") val,
            options(nomem, nostack, preserves_flags),
        );
    }
    val
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

// ── Virtqueue layout helpers ──────────────────────────────────────────────────
//
// All offsets are relative to `vq_phys`, the physical base of the virtqueue
// allocation.  Since phys == virt in the identity map, these are also valid
// virtual addresses.

/// Physical address of descriptor `i` (16-byte `VirtqDesc` struct).
#[inline]
fn desc_pa(vq: u64, i: u16) -> u64 {
    vq + i as u64 * 16
}

/// Physical address of `avail.flags`.
#[inline]
fn avail_pa(vq: u64, q: u16) -> u64 {
    vq + q as u64 * 16  // immediately after all descriptors
}

/// Physical address of `avail.idx`.
#[inline]
fn avail_idx_pa(vq: u64, q: u16) -> u64 {
    avail_pa(vq, q) + 2
}

/// Physical address of `avail.ring[slot]` (slot = avail_idx % q).
#[inline]
fn avail_ring_pa(vq: u64, q: u16, slot: u16) -> u64 {
    avail_pa(vq, q) + 4 + slot as u64 * 2
}


// ── Driver state ──────────────────────────────────────────────────────────────

struct VirtioBlkDev {
    io_base:   u16,  // BAR0 I/O port base
    q_size:    u16,  // negotiated virtqueue size (≤ QUEUE_SIZE_MAX)
    capacity:  u64,  // total 512-byte sectors on the disk
    vq_phys:   u64,  // physical (= virtual) base of the virtqueue allocation
    hdr_phys:  u64,  // physical (= virtual) base of request-header DMA frame
    dat_phys:  u64,  // physical (= virtual) base of sector-data DMA frame
    avail_idx: u16,  // next producer index to write into the avail ring
    last_used: u16,  // consumer index: number of used-ring entries processed
}

// ── Global driver state wrapper ───────────────────────────────────────────────

/// Interior-mutable container for the global driver instance.
///
/// This kernel is single-threaded; no locking is required.  `UnsafeCell`
/// avoids the Rust 2024 `static_mut_refs` lint while keeping the semantics
/// of a plain `static mut`.
struct DevState(core::cell::UnsafeCell<Option<VirtioBlkDev>>);

// SAFETY: single-threaded kernel — no concurrent access.
unsafe impl Sync for DevState {}

static DEV: DevState = DevState(core::cell::UnsafeCell::new(None));

/// Borrow the device mutably.
#[inline]
fn dev_mut() -> Option<&'static mut VirtioBlkDev> {
    unsafe { (*DEV.0.get()).as_mut() }
}

/// Borrow the device immutably.
#[inline]
fn dev_ref() -> Option<&'static VirtioBlkDev> {
    unsafe { (*DEV.0.get()).as_ref() }
}

// ── Virtqueue descriptor write ────────────────────────────────────────────────

impl VirtioBlkDev {
    /// Write one 16-byte descriptor entry.
    unsafe fn write_desc(&self, idx: u16, phys: u64, len: u32, flags: u16, next: u16) {
        let base = desc_pa(self.vq_phys, idx) as *mut u8;
        unsafe {
            (base        as *mut u64).write_volatile(phys);
            (base.add(8) as *mut u32).write_volatile(len);
            (base.add(12) as *mut u16).write_volatile(flags);
            (base.add(14) as *mut u16).write_volatile(next);
        }
    }

    /// Submit a single-sector read or write.
    ///
    /// Returns `true` on success (device status byte == 0).
    fn submit(&mut self, sector: u64, write: bool) -> bool {
        let rflags: u64;
        unsafe { core::arch::asm!("pushfq; pop {0}", out(reg) rflags, options(nomem)) };

        // ── Build request header at hdr_phys ─────────────────────────────────
        // VirtioBlkReq layout (16 bytes):
        //   [0..4)  type   (u32)
        //   [4..8)  ioprio (u32)
        //   [8..16) sector (u64)
        let hdr = self.hdr_phys as *mut u8;
        unsafe {
            (hdr        as *mut u32).write_volatile(
                if write { VIRTIO_BLK_T_OUT } else { VIRTIO_BLK_T_IN }
            );
            (hdr.add(4) as *mut u32).write_volatile(0); // ioprio
            (hdr.add(8) as *mut u64).write_volatile(sector);
        }

        // Status byte follows the 16-byte header in the same DMA frame.
        let status_pa = self.hdr_phys + 16;
        unsafe { (status_pa as *mut u8).write_volatile(0xFF) }; // poison → device overwrites

        // ── Build 3-descriptor chain ─────────────────────────────────────────
        //
        // desc[0]: request header (device reads)  → NEXT → desc[1]
        // desc[1]: data buffer
        //          read : WRITE | NEXT (device writes sector data)
        //          write: NEXT        (device reads from our buffer)
        // desc[2]: status byte (device writes)    → no NEXT
        let data_flags = if write {
            VIRTQ_DESC_F_NEXT                       // device reads data
        } else {
            VIRTQ_DESC_F_NEXT | VIRTQ_DESC_F_WRITE  // device writes data
        };
        unsafe {
            self.write_desc(0, self.hdr_phys,  16,          VIRTQ_DESC_F_NEXT, 1);
            self.write_desc(1, self.dat_phys,  SECTOR_SIZE as u32, data_flags,  2);
            self.write_desc(2, status_pa,      1,           VIRTQ_DESC_F_WRITE, 0);
        }

        // ── Post descriptor chain to available ring ───────────────────────────
        let slot = self.avail_idx % self.q_size;
        unsafe {
            // avail.ring[slot] = 0  (index of the head descriptor)
            (avail_ring_pa(self.vq_phys, self.q_size, slot) as *mut u16)
                .write_volatile(0);
            // Bump avail.idx (wraps at u16 max — matches VirtIO spec)
            (avail_idx_pa(self.vq_phys, self.q_size) as *mut u16)
                .write_volatile(self.avail_idx.wrapping_add(1));
        }
        self.avail_idx = self.avail_idx.wrapping_add(1);

        // Ensure all descriptor and available-ring writes complete before the
        // device reads them via the kick below.
        atomic::fence(Ordering::SeqCst);

        // ── Kick the device ───────────────────────────────────────────────────
        unsafe { outw(self.io_base + REG_QUEUE_NOTIFY, 0) };

        // ── Poll status byte for completion ───────────────────────────────────
        // Status byte was poisoned with 0xFF above; any other value = done.
        // `hlt` yields to QEMU's event loop so AIO completions can fire.
        let expected = self.last_used.wrapping_add(1);
        let status_ptr = status_pa as *const u8;
        let deadline = crate::apic::ticks() + 5000; // 5 s timeout
        loop {
            atomic::fence(Ordering::Acquire);
            let s = unsafe { status_ptr.read_volatile() };
            if s != 0xFF { break; }
            if crate::apic::ticks() >= deadline {
                crate::kprintln!("[virtio-blk] timeout waiting for sector {}", sector);
                unsafe { core::arch::asm!("push {0}; popfq", in(reg) rflags, options(nomem)) };
                return false;
            }
            // sti;hlt: enable interrupts so the virtio IRQ can wake us even
            // when called from syscall context (FMASK clears IF). RFLAGS is
            // restored on return so the caller's IF state is preserved.
            unsafe { core::arch::asm!("sti; hlt", options(nomem, nostack)) };
        }
        self.last_used = expected;

        // Acknowledge any pending virtio-blk IRQ.
        let _ = unsafe { inb(self.io_base + REG_ISR_STATUS) };

        unsafe { core::arch::asm!("push {0}; popfq", in(reg) rflags, options(nomem)) };

        // ── Check device status byte ──────────────────────────────────────────
        let status = unsafe { (status_pa as *const u8).read_volatile() };
        status == 0
    }
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Initialise the VirtIO block driver.
///
/// Scans PCI bus 0 for a VirtIO block device (0x1AF4:0x1001), initialises
/// the device and virtqueue, and stores the driver state globally.
///
/// Returns `true` if a device was found and successfully initialised.
///
/// Must be called after `vmm::init()`, `heap::init()`, and `ioapic::init()`.
pub fn init() -> bool {
    let pci = match crate::pci::find_device(VIRTIO_VENDOR, VIRTIO_BLK_DEV) {
        Some(d) => d,
        None    => return false,
    };

    let io = pci.io_bar0;

    // ── VirtIO device initialisation sequence (legacy spec) ───────────────────
    // 1. Reset device.
    unsafe { outb(io + REG_DEVICE_STATUS, 0) };
    // 2. Acknowledge we found the device.
    unsafe { outb(io + REG_DEVICE_STATUS, STATUS_ACKNOWLEDGE) };
    // 3. Declare driver support.
    unsafe { outb(io + REG_DEVICE_STATUS, STATUS_ACKNOWLEDGE | STATUS_DRIVER) };
    // 4. Read & accept device features (we require none).
    let _features = unsafe { inl(io + REG_DEVICE_FEATURES) };
    unsafe { outl(io + REG_GUEST_FEATURES, 0) };

    // ── Read disk capacity ────────────────────────────────────────────────────
    let cap_lo = unsafe { inl(io + REG_BLK_CAPACITY_LO) };
    let cap_hi = unsafe { inl(io + REG_BLK_CAPACITY_HI) };
    let capacity = (cap_hi as u64) << 32 | cap_lo as u64;

    // ── Configure virtqueue 0 ─────────────────────────────────────────────────
    // Select queue 0.
    unsafe { outw(io + REG_QUEUE_SEL, 0) };

    // Read the max queue size reported by the device.
    let dev_q_size = unsafe {
        let val: u16;
        core::arch::asm!(
            "in ax, dx",
            in("dx") (io + REG_QUEUE_NUM), out("ax") val,
            options(nomem, nostack, preserves_flags),
        );
        val
    };
    if dev_q_size == 0 {
        // Device reports queue unavailable.
        unsafe { outb(io + REG_DEVICE_STATUS, 0x80) }; // STATUS_FAILED
        return false;
    }
    let q_size = dev_q_size.min(QUEUE_SIZE_MAX as u16);

    // Allocate QUEUE_PAGES physically-contiguous 4 KiB frames for the virtqueue.
    // The physical address is also the virtual address (identity map, phys < 1 GiB).
    let vq_phys = match crate::pmm::alloc_frames_contiguous(QUEUE_PAGES) {
        Some(pa) => pa.as_u64(),
        None     => {
            unsafe { outb(io + REG_DEVICE_STATUS, 0x80) };
            return false;
        }
    };
    // Zero the virtqueue pages (spec requires this).
    unsafe {
        core::ptr::write_bytes(vq_phys as *mut u8, 0, QUEUE_PAGES * 4096);
    }

    // Allocate one frame for request header + status byte.
    let hdr_phys = match crate::pmm::alloc_frame() {
        Some(pa) => pa.as_u64(),
        None     => {
            crate::pmm::free_frames_contiguous(crate::pmm::PhysAddr(vq_phys), QUEUE_PAGES);
            unsafe { outb(io + REG_DEVICE_STATUS, 0x80) };
            return false;
        }
    };

    // Allocate one frame for sector data (512 bytes, one sector).
    let dat_phys = match crate::pmm::alloc_frame() {
        Some(pa) => pa.as_u64(),
        None     => {
            crate::pmm::free_frames_contiguous(crate::pmm::PhysAddr(vq_phys), QUEUE_PAGES);
            crate::pmm::free_frame(crate::pmm::PhysAddr(hdr_phys));
            unsafe { outb(io + REG_DEVICE_STATUS, 0x80) };
            return false;
        }
    };

    // Write the virtqueue PFN to the device (physical address / 4096).
    unsafe { outl(io + REG_QUEUE_PFN, (vq_phys / 4096) as u32) };

    // 7. Signal driver ready.
    unsafe {
        outb(io + REG_DEVICE_STATUS,
             STATUS_ACKNOWLEDGE | STATUS_DRIVER | STATUS_DRIVER_OK)
    };

    // Verify device accepted the init sequence (FAILED bit = 0x80 must be clear).
    let dev_status = unsafe { inb(io + REG_DEVICE_STATUS) };
    if dev_status & 0x80 != 0 {
        crate::kprintln!("[virtio-blk] device FAILED status={:#x}", dev_status);
        return false;
    }
    crate::kprintln!("[virtio-blk] device status after DRIVER_OK={:#x}", dev_status);

    // ── Store global state ────────────────────────────────────────────────────
    unsafe {
        *DEV.0.get() = Some(VirtioBlkDev {
            io_base: io,
            q_size,
            capacity,
            vq_phys,
            hdr_phys,
            dat_phys,
            avail_idx: 0,
            last_used: 0,
        });
    }

    // ── Wire the PCI interrupt through the I/O APIC ───────────────────────────
    // PCI IRQs are active-low level-triggered.  The IRQ line from PCI config
    // space is the GSI; map it to our vector so QEMU can wake the CPU via the
    // virtio-blk interrupt instead of waiting for the 1 ms APIC timer tick.
    crate::idt::register_irq(
        VECTOR_VIRTIO_BLK,
        virtio_blk_isr_stub as *const () as u64,
    );
    crate::ioapic::map_irq(
        pci.irq_line,
        VECTOR_VIRTIO_BLK,
        crate::ioapic::IRQ_LEVEL | crate::ioapic::IRQ_ACTIVE_LO,
    );

    true
}

/// Read one 512-byte sector from the disk into `buf`.
///
/// Returns `true` on success, `false` on device error or if no device is present.
pub fn read_sector(sector: u64, buf: &mut [u8; SECTOR_SIZE]) -> bool {
    let dev = match dev_mut() {
        Some(d) => d,
        None    => return false,
    };
    if sector >= dev.capacity { return false; }
    if !dev.submit(sector, false) { return false; }
    // Copy from DMA buffer to caller.
    unsafe {
        core::ptr::copy_nonoverlapping(
            dev.dat_phys as *const u8,
            buf.as_mut_ptr(),
            SECTOR_SIZE,
        );
    }
    true
}

/// Write one 512-byte sector from `buf` to the disk.
///
/// Returns `true` on success, `false` on device error or if no device is present.
pub fn write_sector(sector: u64, buf: &[u8; SECTOR_SIZE]) -> bool {
    let dev = match dev_mut() {
        Some(d) => d,
        None    => return false,
    };
    if sector >= dev.capacity { return false; }
    // Copy from caller into DMA buffer before submitting.
    unsafe {
        core::ptr::copy_nonoverlapping(
            buf.as_ptr(),
            dev.dat_phys as *mut u8,
            SECTOR_SIZE,
        );
    }
    dev.submit(sector, true)
}

/// Return the disk capacity in 512-byte sectors, or 0 if no device.
pub fn capacity_sectors() -> u64 {
    dev_ref().map_or(0, |d| d.capacity)
}

/// Return `true` if a VirtIO block device was found and initialised.
pub fn is_present() -> bool {
    dev_ref().is_some()
}
