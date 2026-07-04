//! VirtIO legacy network device driver (virtio-net, PCI transport).
//!
//! Provides raw Ethernet send/receive. The net stack in `net/mod.rs` sits on top.
//!
//! ## Queue layout
//!
//! Two virtqueues:
//! - Queue 0 (RX): pre-populated with receive buffers. Device writes packets in.
//! - Queue 1 (TX): driver submits frames. Device reads and sends them.
//!
//! Ring sizes come from the device (`REG_QUEUE_NUM`, legacy interface) — the
//! device computes its used-ring offset and index modulus from that value, so
//! the driver must use it too.  RX buffers are decoupled from the ring size:
//! `NUM_RX_BUFS` MTU-sized buffers packed at `RX_BUF_STRIDE` intervals in one
//! contiguous physical pool.  The first 10 bytes of each buffer are a
//! `VirtioNetHdr`; bytes 10..10+frame_len are the Ethernet frame.
//!
//! TX uses two descriptors per packet (10-byte zero header + frame data),
//! both pointing into a single shared page.

use core::arch::global_asm;
use core::sync::atomic::{self, Ordering};

// ── IRQ vector ────────────────────────────────────────────────────────────────

pub const VECTOR_VIRTIO_NET: u8 = 35;

// ── ISR stub ─────────────────────────────────────────────────────────────────

global_asm!(r#"
.section .text
.global virtio_net_isr_stub
.type   virtio_net_isr_stub, @function
virtio_net_isr_stub:
    pushq  %rax
    pushq  %rcx
    pushq  %rdx
    pushq  %rsi
    pushq  %rdi
    pushq  %r8
    pushq  %r9
    pushq  %r10
    pushq  %r11
    call   virtio_net_irq_handler
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

unsafe extern "C" { fn virtio_net_isr_stub(); }

/// Called from `virtio_net_isr_stub` on every virtio-net PCI interrupt.
#[unsafe(no_mangle)]
pub extern "C" fn virtio_net_irq_handler() {
    if let Some(dev) = dev_mut() {
        let _ = unsafe { inb(dev.io_base + REG_ISR_STATUS) };
        dev.drain_rx();
    }
    crate::apic::eoi();
}

// ── PCI IDs ───────────────────────────────────────────────────────────────────

const VIRTIO_VENDOR:   u16 = 0x1AF4;
const VIRTIO_NET_DEV:  u16 = 0x1000;

// ── VirtIO legacy I/O register offsets ───────────────────────────────────────

const REG_DEVICE_FEATURES: u16 = 0x00;
const REG_GUEST_FEATURES:  u16 = 0x04;
const REG_QUEUE_PFN:       u16 = 0x08;
const REG_QUEUE_NUM:       u16 = 0x0C;
const REG_QUEUE_SEL:       u16 = 0x0E;
const REG_QUEUE_NOTIFY:    u16 = 0x10;
const REG_DEVICE_STATUS:   u16 = 0x12;
const REG_ISR_STATUS:      u16 = 0x13;
// virtio-net device config starts at 0x14: 6-byte MAC
const REG_MAC:             u16 = 0x14;

// ── Device status flags ───────────────────────────────────────────────────────

const STATUS_ACKNOWLEDGE: u8 = 1;
const STATUS_DRIVER:      u8 = 2;
const STATUS_DRIVER_OK:   u8 = 4;

// ── Virtqueue descriptor flags ────────────────────────────────────────────────

const VIRTQ_DESC_F_NEXT:  u16 = 1;
const VIRTQ_DESC_F_WRITE: u16 = 2;

// ── Sizing ────────────────────────────────────────────────────────────────────

/// Size of the VirtIO net header (legacy, no VIRTIO_NET_F_MRG_RXBUF).
pub const NET_HDR_SIZE: usize = 10;

/// Maximum Ethernet frame size (without FCS).
pub const MAX_FRAME: usize = 1514;

/// Buffer size for each RX slot: net header + one full Ethernet frame.
const RX_BUF_SIZE: usize = NET_HDR_SIZE + MAX_FRAME; // 1524

/// Number of RX buffers pre-posted to the device.  Independent of the ring
/// size: only this many descriptors are ever in flight.  32 × 1.5 KiB absorbs
/// a full RX_FIFO worth of backlog before the device starts dropping.
const NUM_RX_BUFS: usize = 32;

/// Spacing of RX buffers inside the contiguous pool.  2 KiB keeps each
/// 1524-byte buffer within aligned bounds → pool = 16 pages for 32 buffers
/// (previously 64 buffers × one full 4 KiB page each = 64 pages).
const RX_BUF_STRIDE: usize = 2048;

/// Pages for the RX buffer pool.
const RX_POOL_PAGES: usize = NUM_RX_BUFS * RX_BUF_STRIDE / 4096;

// ── Kernel RX FIFO ────────────────────────────────────────────────────────────
//
// Received frames (stripped of the VirtIO header) land here before the net task
// processes them.  Size in bytes for the payload data (without the net header).

const RX_FIFO_SLOTS: usize = 32;

#[repr(C)]
#[derive(Copy, Clone)]
struct RxSlot {
    data: [u8; MAX_FRAME],
    len:  usize,
}

struct RxFifo {
    slots: [RxSlot; RX_FIFO_SLOTS],
    head:  usize,
    tail:  usize,
}

impl RxFifo {
    const fn new() -> Self {
        Self {
            slots: [RxSlot { data: [0; MAX_FRAME], len: 0 }; RX_FIFO_SLOTS],
            head:  0,
            tail:  0,
        }
    }

    fn push(&mut self, data: &[u8]) -> bool {
        let next = (self.tail + 1) % RX_FIFO_SLOTS;
        if next == self.head { return false; } // full
        let slot = &mut self.slots[self.tail];
        let n = data.len().min(MAX_FRAME);
        slot.data[..n].copy_from_slice(&data[..n]);
        slot.len = n;
        self.tail = next;
        true
    }

    fn pop(&mut self, buf: &mut [u8]) -> Option<usize> {
        if self.head == self.tail { return None; }
        let slot = &self.slots[self.head];
        let n = slot.len.min(buf.len());
        buf[..n].copy_from_slice(&slot.data[..n]);
        self.head = (self.head + 1) % RX_FIFO_SLOTS;
        Some(n)
    }

    #[allow(dead_code)]
    fn is_empty(&self) -> bool { self.head == self.tail }
}

struct FifoState(core::cell::UnsafeCell<RxFifo>);
unsafe impl Sync for FifoState {}
static RX_FIFO: FifoState = FifoState(core::cell::UnsafeCell::new(RxFifo::new()));

fn rx_fifo() -> &'static mut RxFifo {
    unsafe { &mut *RX_FIFO.0.get() }
}

// ── Port I/O helpers ──────────────────────────────────────────────────────────

#[inline]
unsafe fn outb(port: u16, val: u8) {
    unsafe { core::arch::asm!("out dx, al", in("dx") port, in("al") val,
        options(nomem, nostack, preserves_flags)) }
}
#[inline]
unsafe fn outw(port: u16, val: u16) {
    unsafe { core::arch::asm!("out dx, ax", in("dx") port, in("ax") val,
        options(nomem, nostack, preserves_flags)) }
}
#[inline]
unsafe fn outl(port: u16, val: u32) {
    unsafe { core::arch::asm!("out dx, eax", in("dx") port, in("eax") val,
        options(nomem, nostack, preserves_flags)) }
}
#[inline]
unsafe fn inb(port: u16) -> u8 {
    let val: u8;
    unsafe { core::arch::asm!("in al, dx", in("dx") port, out("al") val,
        options(nomem, nostack, preserves_flags)) }
    val
}
#[inline]
unsafe fn inl(port: u16) -> u32 {
    let val: u32;
    unsafe { core::arch::asm!("in eax, dx", in("dx") port, out("eax") val,
        options(nomem, nostack, preserves_flags)) }
    val
}

// ── Virtqueue helpers ─────────────────────────────────────────────────────────
//
// Legacy layout for a queue of size `qsz` (virtio 0.9.5 / legacy interface):
//   offset 0                — descriptor table, 16 × qsz bytes
//   offset 16·qsz           — avail ring: flags u16, idx u16, ring[qsz] u16
//   align↑4096              — used ring:  flags u16, idx u16, ring[qsz] × 8 B

#[inline] fn desc_pa(vq: u64, i: u16) -> u64 { vq + i as u64 * 16 }
#[inline] fn avail_pa(vq: u64, qsz: u16)     -> u64 { vq + qsz as u64 * 16 }
#[inline] fn avail_idx_pa(vq: u64, qsz: u16) -> u64 { avail_pa(vq, qsz) + 2 }
#[inline] fn avail_ring_pa(vq: u64, qsz: u16, slot: u16) -> u64 {
    avail_pa(vq, qsz) + 4 + slot as u64 * 2
}
#[inline] fn used_pa(vq: u64, qsz: u16) -> u64 {
    let avail_end = avail_pa(vq, qsz) + 4 + qsz as u64 * 2;
    (avail_end + 4095) & !4095
}
#[inline] fn used_idx_pa(vq: u64, qsz: u16) -> u64 { used_pa(vq, qsz) + 2 }
#[inline] fn used_ring_pa(vq: u64, qsz: u16, slot: u16) -> u64 {
    used_pa(vq, qsz) + 4 + slot as u64 * 8
}

/// Total pages needed for one legacy virtqueue of size `qsz`.
fn queue_pages(qsz: u16) -> usize {
    let end = used_pa(0, qsz) + 4 + qsz as u64 * 8;
    ((end + 4095) / 4096) as usize
}

// ── Driver state ──────────────────────────────────────────────────────────────

struct VirtioNetDev {
    io_base:    u16,
    mac:        [u8; 6],

    // RX virtqueue
    rx_vq:      u64,   // phys base of RX virtqueue pages
    rx_qsz:     u16,   // ring size reported by the device
    rx_bufs:    [u64; NUM_RX_BUFS], // phys of each RX buffer (packed pool)
    rx_avail:   u16,   // next avail.idx to produce
    rx_used:    u16,   // last used.idx consumed

    // TX virtqueue
    tx_vq:      u64,   // phys base of TX virtqueue pages
    tx_qsz:     u16,   // ring size reported by the device
    tx_avail:   u16,   // next avail.idx to produce
    tx_used:    u16,   // last used.idx consumed
    tx_hdr:     u64,   // phys of 10-byte TX header (offset 0 of shared page)
    tx_dat:     u64,   // phys of TX data (offset 16 of the same page)
}

impl VirtioNetDev {
    unsafe fn write_desc(&self, vq: u64, idx: u16, phys: u64, len: u32, flags: u16, next: u16) {
        let base = desc_pa(vq, idx) as *mut u8;
        unsafe {
            (base        as *mut u64).write_volatile(phys);
            (base.add(8) as *mut u32).write_volatile(len);
            (base.add(12) as *mut u16).write_volatile(flags);
            (base.add(14) as *mut u16).write_volatile(next);
        }
    }

    /// Drain used RX ring entries, copy frames to RX_FIFO, recycle descriptors.
    fn drain_rx(&mut self) {
        loop {
            atomic::fence(Ordering::Acquire);
            let used_idx = unsafe {
                (used_idx_pa(self.rx_vq, self.rx_qsz) as *const u16).read_volatile()
            };
            if self.rx_used == used_idx { break; }

            let slot = self.rx_used % self.rx_qsz;
            let used_elem_pa = used_ring_pa(self.rx_vq, self.rx_qsz, slot);
            let desc_id = unsafe { (used_elem_pa as *const u32).read_volatile() } as usize;
            let bytes   = unsafe { ((used_elem_pa + 4) as *const u32).read_volatile() } as usize;

            self.rx_used = self.rx_used.wrapping_add(1);

            // Only descriptors 0..NUM_RX_BUFS are ever posted; anything else
            // is device misbehaviour — drop it rather than index out of range.
            if desc_id >= NUM_RX_BUFS { continue; }

            // Frame data starts after the 10-byte virtio net header.
            let buf_phys = self.rx_bufs[desc_id];
            let frame_start = buf_phys as usize + NET_HDR_SIZE;
            let frame_len   = bytes.saturating_sub(NET_HDR_SIZE).min(MAX_FRAME);

            if frame_len > 0 {
                let frame = unsafe {
                    core::slice::from_raw_parts(frame_start as *const u8, frame_len)
                };
                rx_fifo().push(frame);
            }

            // Recycle: put this descriptor back into the available ring.
            let avail_slot = self.rx_avail % self.rx_qsz;
            unsafe {
                (avail_ring_pa(self.rx_vq, self.rx_qsz, avail_slot) as *mut u16)
                    .write_volatile(desc_id as u16);
                self.rx_avail = self.rx_avail.wrapping_add(1);
                atomic::fence(Ordering::Release);
                (avail_idx_pa(self.rx_vq, self.rx_qsz) as *mut u16)
                    .write_volatile(self.rx_avail);
            }
            unsafe { outw(self.io_base + REG_QUEUE_NOTIFY, 0) };
        }
    }

    /// Send an Ethernet frame. Returns true on success.
    fn send_frame(&mut self, frame: &[u8]) -> bool {
        if frame.len() > MAX_FRAME { return false; }

        // Write zero VirtIO net header.
        unsafe {
            core::ptr::write_bytes(self.tx_hdr as *mut u8, 0, NET_HDR_SIZE);
        }
        // Copy frame data.
        let len = frame.len();
        unsafe {
            core::ptr::copy_nonoverlapping(frame.as_ptr(), self.tx_dat as *mut u8, len);
        }

        // Build two-descriptor chain: header + data.
        unsafe {
            self.write_desc(self.tx_vq, 0, self.tx_hdr, NET_HDR_SIZE as u32,
                            VIRTQ_DESC_F_NEXT, 1);
            self.write_desc(self.tx_vq, 1, self.tx_dat, len as u32, 0, 0);
        }

        let slot = self.tx_avail % self.tx_qsz;
        unsafe {
            (avail_ring_pa(self.tx_vq, self.tx_qsz, slot) as *mut u16).write_volatile(0);
            self.tx_avail = self.tx_avail.wrapping_add(1);
            atomic::fence(Ordering::Release);
            (avail_idx_pa(self.tx_vq, self.tx_qsz) as *mut u16).write_volatile(self.tx_avail);
        }
        unsafe { outw(self.io_base + REG_QUEUE_NOTIFY, 1) };

        // Wait for TX completion (poll used ring).
        let deadline = crate::apic::ticks() + 500;
        loop {
            atomic::fence(Ordering::Acquire);
            let used_idx = unsafe { (used_idx_pa(self.tx_vq, self.tx_qsz) as *const u16).read_volatile() };
            if self.tx_used != used_idx {
                self.tx_used = self.tx_used.wrapping_add(1);
                return true;
            }
            if crate::apic::ticks() >= deadline {
                return false;
            }
            unsafe { core::arch::asm!("sti; hlt", options(nomem, nostack)) };
        }
    }
}

// ── Global driver state ───────────────────────────────────────────────────────

struct DevState(core::cell::UnsafeCell<Option<VirtioNetDev>>);
unsafe impl Sync for DevState {}
static DEV: DevState = DevState(core::cell::UnsafeCell::new(None));

fn dev_mut() -> Option<&'static mut VirtioNetDev> {
    unsafe { (*DEV.0.get()).as_mut() }
}
fn dev_ref() -> Option<&'static VirtioNetDev> {
    unsafe { (*DEV.0.get()).as_ref() }
}

// ── Virtqueue setup helper ────────────────────────────────────────────────────

fn setup_queue(io: u16, queue_idx: u16) -> Option<(u64, u16)> {
    unsafe { outw(io + REG_QUEUE_SEL, queue_idx) };
    let q_num = unsafe { inl(io + REG_QUEUE_NUM) } as u16;
    if q_num == 0 { return None; }

    // Legacy interface: the ring size is fixed by the device and the device
    // derives its used-ring offset and index modulus from it — allocate and
    // index exactly to q_num.
    let pages = queue_pages(q_num);
    let vq_phys = crate::pmm::alloc_frames_contiguous(pages)?.as_u64();
    unsafe { core::ptr::write_bytes(vq_phys as *mut u8, 0, pages * 4096) };
    unsafe { outl(io + REG_QUEUE_PFN, (vq_phys / 4096) as u32) };
    Some((vq_phys, q_num))
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Initialise the VirtIO network driver.
///
/// Returns `true` if a virtio-net device was found and configured.
/// Must be called after `vmm::init()`, `heap::init()`, and `ioapic::init()`.
pub fn init() -> bool {
    let pci = match crate::pci::find_device(VIRTIO_VENDOR, VIRTIO_NET_DEV) {
        Some(d) => d,
        None    => return false,
    };
    let io = pci.io_bar0;

    // ── VirtIO legacy init sequence ───────────────────────────────────────────
    unsafe { outb(io + REG_DEVICE_STATUS, 0) };
    unsafe { outb(io + REG_DEVICE_STATUS, STATUS_ACKNOWLEDGE) };
    unsafe { outb(io + REG_DEVICE_STATUS, STATUS_ACKNOWLEDGE | STATUS_DRIVER) };

    // Accept no features (basic frames only — no GSO, no checksum offload).
    let _features = unsafe { inl(io + REG_DEVICE_FEATURES) };
    unsafe { outl(io + REG_GUEST_FEATURES, 0) };

    // ── Read MAC address ──────────────────────────────────────────────────────
    let mut mac = [0u8; 6];
    for (i, b) in mac.iter_mut().enumerate() {
        *b = unsafe { inb(io + REG_MAC + i as u16) };
    }

    // ── Setup RX queue (queue 0) ──────────────────────────────────────────────
    let (rx_vq, rx_qsz) = match setup_queue(io, 0) {
        Some(v) => v,
        None    => { unsafe { outb(io + REG_DEVICE_STATUS, 0x80) }; return false; }
    };

    // RX buffer pool: NUM_RX_BUFS MTU-sized buffers packed at RX_BUF_STRIDE
    // in one contiguous physical run.  Never post more buffers than the ring
    // can hold (rx_qsz is ≥ 256 on QEMU, so this is a formality).
    let n_bufs = NUM_RX_BUFS.min(rx_qsz as usize);
    let rx_pool = match crate::pmm::alloc_frames_contiguous(RX_POOL_PAGES) {
        Some(pa) => pa.as_u64(),
        None     => { unsafe { outb(io + REG_DEVICE_STATUS, 0x80) }; return false; }
    };
    unsafe { (rx_pool as *mut u8).write_bytes(0, RX_POOL_PAGES * 4096) };

    let mut rx_bufs = [0u64; NUM_RX_BUFS];
    for (i, buf) in rx_bufs.iter_mut().enumerate().take(n_bufs) {
        *buf = rx_pool + (i * RX_BUF_STRIDE) as u64;
        // Write descriptor: WRITE (device writes into this buffer), no NEXT.
        let desc_base = desc_pa(rx_vq, i as u16) as *mut u8;
        unsafe {
            (desc_base        as *mut u64).write_volatile(*buf);
            (desc_base.add(8) as *mut u32).write_volatile(RX_BUF_SIZE as u32);
            (desc_base.add(12) as *mut u16).write_volatile(VIRTQ_DESC_F_WRITE);
            (desc_base.add(14) as *mut u16).write_volatile(0);
            // Add to available ring (i < n_bufs ≤ rx_qsz, no wrap needed).
            (avail_ring_pa(rx_vq, rx_qsz, i as u16) as *mut u16).write_volatile(i as u16);
        }
    }
    // Publish all RX descriptors.
    unsafe {
        atomic::fence(Ordering::Release);
        (avail_idx_pa(rx_vq, rx_qsz) as *mut u16).write_volatile(n_bufs as u16);
        outw(io + REG_QUEUE_NOTIFY, 0);
    }

    // ── Setup TX queue (queue 1) ──────────────────────────────────────────────
    let (tx_vq, tx_qsz) = match setup_queue(io, 1) {
        Some(v) => v,
        None    => { unsafe { outb(io + REG_DEVICE_STATUS, 0x80) }; return false; }
    };

    // TX DMA buffers: header at offset 0 and payload at offset 16 of one
    // shared page (10 + 1514 bytes fit with room to spare).
    let tx_page = match crate::pmm::alloc_frame() {
        Some(pa) => pa.as_u64(),
        None     => { unsafe { outb(io + REG_DEVICE_STATUS, 0x80) }; return false; }
    };
    let tx_hdr = tx_page;
    let tx_dat = tx_page + 16;

    // ── Signal DRIVER_OK ──────────────────────────────────────────────────────
    unsafe { outb(io + REG_DEVICE_STATUS, STATUS_ACKNOWLEDGE | STATUS_DRIVER | STATUS_DRIVER_OK) };
    let dev_status = unsafe { inb(io + REG_DEVICE_STATUS) };
    if dev_status & 0x80 != 0 {
        crate::kprintln!("[virtio-net] device FAILED status={:#x}", dev_status);
        return false;
    }

    // ── Store global state ────────────────────────────────────────────────────
    unsafe {
        *DEV.0.get() = Some(VirtioNetDev {
            io_base: io, mac,
            rx_vq, rx_qsz, rx_bufs, rx_avail: n_bufs as u16, rx_used: 0,
            tx_vq, tx_qsz, tx_avail: 0, tx_used: 0, tx_hdr, tx_dat,
        });
    }

    // ── Wire IRQ ──────────────────────────────────────────────────────────────
    crate::idt::register_irq(VECTOR_VIRTIO_NET, virtio_net_isr_stub as *const () as u64);
    crate::ioapic::map_irq(
        pci.irq_line as u32,
        VECTOR_VIRTIO_NET,
        crate::ioapic::IRQ_LEVEL | crate::ioapic::IRQ_ACTIVE_LO,
    );

    true
}

/// Send an Ethernet frame. Returns `true` on success.
pub fn send(frame: &[u8]) -> bool {
    match dev_mut() {
        Some(d) => d.send_frame(frame),
        None    => false,
    }
}

/// Try to receive an Ethernet frame from the kernel RX FIFO.
/// Non-blocking: returns `None` if no packet is waiting.
pub fn try_recv(buf: &mut [u8]) -> Option<usize> {
    // First drain the hardware RX ring into the FIFO.
    if let Some(dev) = dev_mut() {
        dev.drain_rx();
    }
    rx_fifo().pop(buf)
}

/// Blocking receive: yields until a frame arrives.
pub fn recv_blocking(buf: &mut [u8]) -> usize {
    loop {
        if let Some(n) = try_recv(buf) {
            return n;
        }
        crate::task::yield_task();
    }
}

/// Return the MAC address of the VirtIO net device, or all-zeros if absent.
pub fn mac_addr() -> [u8; 6] {
    dev_ref().map_or([0u8; 6], |d| d.mac)
}

/// Return `true` if a VirtIO net device was found and initialised.
pub fn is_present() -> bool { dev_ref().is_some() }
