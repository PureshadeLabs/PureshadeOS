//! netd — userspace virtio-net driver (bring-up proof for the device-driver
//! framework).
//!
//! netd is spawned by lythd with a single capability at handle 0: the Device
//! capability for the virtio-net PCI device. Holding only that cap — no ambient
//! authority — it brings the device to *virtqueue-operational*:
//!
//! 1. discover the modern virtio-pci config structures (walking config space
//!    through `SYS_DEV_CFG_READ`),
//! 2. map the device MMIO BAR(s) uncacheable (`SYS_DEV_MMIO_MAP`),
//! 3. negotiate features (VIRTIO_F_VERSION_1),
//! 4. set up the RX and TX split virtqueues in DMA memory
//!    (`SYS_DEV_DMA_ALLOC`),
//! 5. transmit one frame, receive the device IRQ in ring 3
//!    (`SYS_DEV_IRQ_WAIT`), and observe the TX descriptor round-trip via the
//!    used ring.
//!
//! No Ethernet/ARP/IP/protocol logic — that is the next stage. netd prints
//! probe lines (feature bits, IRQ count, descriptor round-trip) proving the
//! framework, then runs a minimal IRQ service loop.

#![no_std]
#![no_main]

use lythos_rt::{println, eprintln, sys_task_exit, task::yield_now,
                sys_dev_mmio_map, sys_dev_irq_wait, sys_dev_irq_ack};
use lythos_driver::{Mmio, dma::DmaPool, virtio_pci, virtq};
use lythos_driver::virtq::{SplitQueue, VIRTQ_DESC_F_WRITE};

/// Device capability handle: lythd hands the virtio-net Device cap at slot 0.
const DEV_CAP: u64 = 0;

/// Base of the per-BAR MMIO virtual window (each BAR gets a 16 MiB slot).
const MMIO_BASE: u64 = 0x0000_0002_0000_0000;
const BAR_SPAN:  u64 = 0x0100_0000;

// ── virtio_pci_common_cfg register offsets ────────────────────────────────────
const CFG_DEV_FEAT_SEL:    u64 = 0x00;
const CFG_DEV_FEAT:        u64 = 0x04;
const CFG_DRV_FEAT_SEL:    u64 = 0x08;
const CFG_DRV_FEAT:        u64 = 0x0C;
const CFG_NUM_QUEUES:      u64 = 0x12;
const CFG_DEV_STATUS:      u64 = 0x14;
const CFG_QUEUE_SEL:       u64 = 0x16;
const CFG_QUEUE_SIZE:      u64 = 0x18;
const CFG_QUEUE_ENABLE:    u64 = 0x1C;
const CFG_QUEUE_NOTIFY_OFF:u64 = 0x1E;
const CFG_QUEUE_DESC:      u64 = 0x20;
const CFG_QUEUE_DRIVER:    u64 = 0x28;
const CFG_QUEUE_DEVICE:    u64 = 0x30;

// ── Device status bits ────────────────────────────────────────────────────────
const S_ACK:         u8 = 1;
const S_DRIVER:      u8 = 2;
const S_DRIVER_OK:   u8 = 4;
const S_FEATURES_OK: u8 = 8;
const S_FAILED:      u8 = 0x80;

/// VIRTIO_F_VERSION_1 (feature bit 32) — bit 0 of the high feature word.
const F_VERSION_1_HI: u32 = 1 << 0;

const RX_QUEUE: u16 = 0;
const TX_QUEUE: u16 = 1;

/// virtio-net header length (modern, with num_buffers): 12 bytes.
const NET_HDR: usize = 12;
/// Ring depth we request (a small power of two suffices for the proof).
const QDEPTH: u16 = 64;
/// RX buffers pre-posted to the device.
const NUM_RX: u16 = 16;
const RX_BUF: u64 = 2048;

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    match bringup() {
        Ok(()) => {}
        Err(e) => {
            eprintln!("[netd] bring-up FAILED: {}", e);
            sys_task_exit();
        }
    }
    sys_task_exit()
}

/// Map a BAR once, returning its virtual base. `mapped[bar]` caches the result.
fn map_bar(bar: u8, mapped: &mut [u64; 6]) -> Result<u64, &'static str> {
    let b = bar as usize;
    if b >= 6 { return Err("bar index out of range"); }
    if mapped[b] != 0 { return Ok(mapped[b]); }
    let virt = MMIO_BASE + bar as u64 * BAR_SPAN;
    match sys_dev_mmio_map(DEV_CAP, bar as u32, virt) {
        Ok(_len) => { mapped[b] = virt; Ok(virt) }
        Err(_)   => Err("SYS_DEV_MMIO_MAP failed (no Device cap or bad BAR)"),
    }
}

fn bringup() -> Result<(), &'static str> {
    println!("[netd] userspace virtio-net driver starting (dev cap handle {})", DEV_CAP);

    // ── 1. Discover modern virtio-pci config structures ───────────────────────
    let lay = virtio_pci::discover(DEV_CAP);
    if !lay.is_modern() {
        return Err("device does not expose modern virtio-pci capabilities");
    }
    println!("[netd] virtio caps: common bar{}+{:#x} notify bar{}+{:#x} (mult {}) isr bar{}+{:#x} device bar{}+{:#x}",
        lay.common.bar, lay.common.offset,
        lay.notify.bar, lay.notify.offset, lay.notify_mult,
        lay.isr.bar, lay.isr.offset,
        lay.device.bar, lay.device.offset);

    // ── 2. Map the BAR(s) uncacheable ─────────────────────────────────────────
    let mut mapped = [0u64; 6];
    let common = Mmio::new(map_bar(lay.common.bar, &mut mapped)? + lay.common.offset as u64);
    let isr    = Mmio::new(map_bar(lay.isr.bar, &mut mapped)? + lay.isr.offset as u64);
    let notify_base = map_bar(lay.notify.bar, &mut mapped)? + lay.notify.offset as u64;
    let device_cfg = if lay.device.present {
        Some(Mmio::new(map_bar(lay.device.bar, &mut mapped)? + lay.device.offset as u64))
    } else { None };

    // ── 3. Reset + feature negotiation ────────────────────────────────────────
    common.write8(CFG_DEV_STATUS, 0);
    while common.read8(CFG_DEV_STATUS) != 0 { yield_now(); }
    common.write8(CFG_DEV_STATUS, S_ACK);
    common.write8(CFG_DEV_STATUS, S_ACK | S_DRIVER);

    common.write32(CFG_DEV_FEAT_SEL, 0);
    let feat_lo = common.read32(CFG_DEV_FEAT);
    common.write32(CFG_DEV_FEAT_SEL, 1);
    let feat_hi = common.read32(CFG_DEV_FEAT);
    println!("[netd] device features: lo={:#010x} hi={:#010x} (VERSION_1={})",
        feat_lo, feat_hi, feat_hi & F_VERSION_1_HI != 0);
    if feat_hi & F_VERSION_1_HI == 0 {
        return Err("device does not offer VIRTIO_F_VERSION_1");
    }

    // Accept only VERSION_1 — no offloads. (feature word 1 = high 32 bits.)
    common.write32(CFG_DRV_FEAT_SEL, 0);
    common.write32(CFG_DRV_FEAT, 0);
    common.write32(CFG_DRV_FEAT_SEL, 1);
    common.write32(CFG_DRV_FEAT, F_VERSION_1_HI);
    println!("[netd] driver features negotiated: VERSION_1 (bit 32)");

    common.write8(CFG_DEV_STATUS, S_ACK | S_DRIVER | S_FEATURES_OK);
    let st = common.read8(CFG_DEV_STATUS);
    if st & S_FEATURES_OK == 0 || st & S_FAILED != 0 {
        return Err("device rejected FEATURES_OK");
    }
    let num_queues = common.read16(CFG_NUM_QUEUES);
    println!("[netd] FEATURES_OK accepted; device reports {} virtqueue(s)", num_queues);

    // ── 4. Set up RX + TX split virtqueues ────────────────────────────────────
    let mut pool = DmaPool::new(DEV_CAP);

    let (mut rxq, _rx_notify) = setup_queue(&common, &mut pool, RX_QUEUE)?;
    let (mut txq, tx_notify)  = setup_queue(&common, &mut pool, TX_QUEUE)?;

    // Pre-post RX buffers: one WRITE descriptor per buffer, published to avail.
    let rx_pool = pool.alloc(NUM_RX as u64 * RX_BUF).map_err(|_| "RX pool DMA alloc failed")?;
    let n_rx = NUM_RX.min(rxq.qsz);
    for i in 0..n_rx {
        let buf_phys = rx_pool.phys + i as u64 * RX_BUF;
        rxq.set_desc(i, buf_phys, RX_BUF as u32, VIRTQ_DESC_F_WRITE, 0);
        rxq.publish(i);
    }
    notify(notify_base, lay.notify_mult, _rx_notify, RX_QUEUE);
    println!("[netd] RX queue: {} buffers posted (qsz {}); TX queue up (qsz {})",
        n_rx, rxq.qsz, txq.qsz);

    // ── 5. DRIVER_OK — the device is live ─────────────────────────────────────
    common.write8(CFG_DEV_STATUS, S_ACK | S_DRIVER | S_FEATURES_OK | S_DRIVER_OK);

    // MAC (device config, if the device-cfg structure is present).
    let mut mac = [0u8; 6];
    if let Some(dc) = device_cfg {
        for (i, b) in mac.iter_mut().enumerate() { *b = dc.read8(i as u64); }
        println!("[netd] MAC {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
            mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]);
    }
    println!("[netd] virtqueues operational — device is DRIVER_OK");

    // ── 6. Proof: TX one frame, take the IRQ in ring 3, see the round-trip ─────
    let tx_buf = pool.alloc(RX_BUF).map_err(|_| "TX DMA alloc failed")?;
    let frame_len = build_probe_frame(tx_buf.as_mut_slice(), &mac);
    // Single read-only descriptor: 12-byte net header + frame, device reads it.
    txq.set_desc(0, tx_buf.phys, (NET_HDR + frame_len) as u32, 0, 0);
    txq.publish(0);
    notify(notify_base, lay.notify_mult, tx_notify, TX_QUEUE);
    println!("[netd] TX submitted: {} byte frame (hdr {} + payload {})",
        NET_HDR + frame_len, NET_HDR, frame_len);

    // Wait for the device IRQ and confirm the TX descriptor completed.
    let mut irq_count = 0u32;
    let mut round_tripped = false;
    for _ in 0..8 {
        sys_dev_irq_wait(DEV_CAP).map_err(|_| "SYS_DEV_IRQ_WAIT failed")?;
        irq_count += 1;
        let _isr = isr.read8(0); // read to deassert the device's INTx line
        if let Some((id, len)) = txq.take_used() {
            println!("[netd] IRQ #{}: TX descriptor round-trip — desc_id={} used_len={}",
                irq_count, id, len);
            round_tripped = true;
            let _ = sys_dev_irq_ack(DEV_CAP);
            break;
        }
        // Not the TX completion (spurious / RX) — drain RX and keep waiting.
        while let Some((id, len)) = rxq.take_used() {
            println!("[netd] IRQ #{}: RX frame on desc {} ({} bytes)", irq_count, id, len);
        }
        let _ = sys_dev_irq_ack(DEV_CAP);
    }
    if !round_tripped {
        return Err("no TX completion IRQ observed");
    }
    println!("[netd] BRING-UP COMPLETE — MMIO mapped, features negotiated, RX/TX queues up, IRQ received in ring 3, descriptor round-tripped");
    println!("[netd] probe summary: irq_count={} tx_round_trip=yes", irq_count);

    // ── Minimal service loop: keep servicing device IRQs (drain RX) ────────────
    service_loop(&isr, &mut rxq);
}

/// Select, size, DMA-allocate, and enable one virtqueue. Returns the queue and
/// its `queue_notify_off`.
fn setup_queue(
    common: &Mmio, pool: &mut DmaPool, q: u16,
) -> Result<(SplitQueue, u16), &'static str> {
    common.write16(CFG_QUEUE_SEL, q);
    let dev_qsz = common.read16(CFG_QUEUE_SIZE);
    if dev_qsz == 0 { return Err("queue unavailable"); }
    let qsz = dev_qsz.min(QDEPTH);
    common.write16(CFG_QUEUE_SIZE, qsz);

    let buf = pool.alloc(virtq::bytes(qsz)).map_err(|_| "virtqueue DMA alloc failed")?;
    let sq = SplitQueue::new(buf, qsz);

    common.write64(CFG_QUEUE_DESC, sq.desc_phys);
    common.write64(CFG_QUEUE_DRIVER, sq.avail_phys);
    common.write64(CFG_QUEUE_DEVICE, sq.used_phys);
    let notify_off = common.read16(CFG_QUEUE_NOTIFY_OFF);
    common.write16(CFG_QUEUE_ENABLE, 1);
    Ok((sq, notify_off))
}

/// Notify the device that `queue` has new available buffers (modern notify:
/// write the queue index at `notify_base + notify_off * notify_mult`).
fn notify(notify_base: u64, notify_mult: u32, notify_off: u16, queue: u16) {
    let addr = notify_base + notify_off as u64 * notify_mult as u64;
    unsafe { (addr as *mut u16).write_volatile(queue) };
}

/// Build a minimal Ethernet probe frame after the 12-byte net header. SLIRP
/// drops it, but the device still completes the descriptor — that is the proof.
fn build_probe_frame(buf: &mut [u8], mac: &[u8; 6]) -> usize {
    // Zero the net header.
    for b in buf[..NET_HDR].iter_mut() { *b = 0; }
    let f = &mut buf[NET_HDR..];
    // dst = broadcast, src = our MAC, ethertype = 0x88B5 (local experimental).
    for b in f[0..6].iter_mut() { *b = 0xFF; }
    f[6..12].copy_from_slice(mac);
    f[12] = 0x88; f[13] = 0xB5;
    // 46-byte zero payload → minimum 60-byte Ethernet frame.
    for b in f[14..60].iter_mut() { *b = 0; }
    60
}

/// After bring-up, block on device IRQs and drain the RX ring (no protocol).
fn service_loop(isr: &Mmio, rxq: &mut SplitQueue) -> ! {
    loop {
        if sys_dev_irq_wait(DEV_CAP).is_err() { yield_now(); continue; }
        let _isr = isr.read8(0);
        while let Some((id, len)) = rxq.take_used() {
            println!("[netd] RX frame on desc {} ({} bytes)", id, len);
            // Re-post the buffer so the device can reuse it.
            rxq.publish(id as u16);
        }
        let _ = sys_dev_irq_ack(DEV_CAP);
    }
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo<'_>) -> ! {
    lythos_rt::sys_log("[netd] PANIC");
    if let Some(msg) = info.message().as_str() {
        lythos_rt::sys_log(": ");
        lythos_rt::sys_log(msg);
    }
    lythos_rt::sys_log("\n");
    sys_task_exit()
}
