//! `Nic` — the virtio-net device wrapped as a frame-level interface for the
//! protocol stack.
//!
//! [`Nic::bringup`] performs the modern virtio-pci handshake (discover, map MMIO,
//! negotiate `VIRTIO_F_VERSION_1`, set up the RX/TX split virtqueues, pre-post RX
//! buffers, `DRIVER_OK`) — the same sequence the framework proved during device
//! bring-up. Once live, [`Nic::recv_into`] copies one received frame out (RX is
//! driven by [`Nic::wait_irq`] from the ring-3 IRQ path) and the [`FrameSink`]
//! impl transmits a frame. All buffers flow through the framework DMA pool.

use lythos_rt::{
    eprintln, println, sys_dev_irq_ack, sys_dev_irq_wait, sys_dev_mmio_map, task::yield_now,
};
use lythos_driver::virtq::{SplitQueue, VIRTQ_DESC_F_WRITE};
use lythos_driver::{dma::DmaPool, virtio_pci, virtq, Mmio};

use crate::net::wire::FrameSink;

/// Device capability handle: lythd hands the virtio-net Device cap at slot 0.
pub const DEV_CAP: u64 = 0;

/// Base of the per-BAR MMIO virtual window (each BAR gets a 16 MiB slot).
const MMIO_BASE: u64 = 0x0000_0002_0000_0000;
const BAR_SPAN: u64 = 0x0100_0000;

// ── virtio_pci_common_cfg register offsets ──────────────────────────────────
const CFG_DEV_FEAT_SEL: u64 = 0x00;
const CFG_DEV_FEAT: u64 = 0x04;
const CFG_DRV_FEAT_SEL: u64 = 0x08;
const CFG_DRV_FEAT: u64 = 0x0C;
const CFG_NUM_QUEUES: u64 = 0x12;
const CFG_DEV_STATUS: u64 = 0x14;
const CFG_QUEUE_SEL: u64 = 0x16;
const CFG_QUEUE_SIZE: u64 = 0x18;
const CFG_QUEUE_ENABLE: u64 = 0x1C;
const CFG_QUEUE_NOTIFY_OFF: u64 = 0x1E;
const CFG_QUEUE_DESC: u64 = 0x20;
const CFG_QUEUE_DRIVER: u64 = 0x28;
const CFG_QUEUE_DEVICE: u64 = 0x30;

// ── Device status bits ───────────────────────────────────────────────────────
const S_ACK: u8 = 1;
const S_DRIVER: u8 = 2;
const S_DRIVER_OK: u8 = 4;
const S_FEATURES_OK: u8 = 8;
const S_FAILED: u8 = 0x80;

/// VIRTIO_F_VERSION_1 (feature bit 32) — bit 0 of the high feature word.
const F_VERSION_1_HI: u32 = 1 << 0;

const RX_QUEUE: u16 = 0;
const TX_QUEUE: u16 = 1;

/// virtio-net header length (modern virtio_net_hdr_v1, with num_buffers): 12 bytes.
const NET_HDR: usize = 12;
/// Ring depth we request (a small power of two suffices).
const QDEPTH: u16 = 64;
/// RX buffers pre-posted to the device.
const NUM_RX: u16 = 16;
/// TX buffers / descriptor slots.
const NUM_TX: u16 = 8;
/// Per-buffer size (holds the 12-byte net header + a full 1500-byte frame).
const BUF_SIZE: u64 = 2048;

/// The virtio-net device as a frame interface.
pub struct Nic {
    isr: Mmio,
    notify_base: u64,
    notify_mult: u32,
    rx_notify_off: u16,
    tx_notify_off: u16,
    rxq: SplitQueue,
    txq: SplitQueue,
    rx_pool: lythos_driver::dma::DmaBuf,
    tx_pool: lythos_driver::dma::DmaBuf,
    tx_free: [u16; NUM_TX as usize],
    tx_free_len: usize,
    pub mac: [u8; 6],
}

/// Map a BAR once, returning its virtual base. `mapped[bar]` caches the result.
fn map_bar(bar: u8, mapped: &mut [u64; 6]) -> Result<u64, &'static str> {
    let b = bar as usize;
    if b >= 6 {
        return Err("bar index out of range");
    }
    if mapped[b] != 0 {
        return Ok(mapped[b]);
    }
    let virt = MMIO_BASE + bar as u64 * BAR_SPAN;
    match sys_dev_mmio_map(DEV_CAP, bar as u32, virt) {
        Ok(_len) => {
            mapped[b] = virt;
            Ok(virt)
        }
        Err(_) => Err("SYS_DEV_MMIO_MAP failed (no Device cap or bad BAR)"),
    }
}

impl Nic {
    /// Bring the device to virtqueue-operational and return the live NIC.
    pub fn bringup() -> Result<Nic, &'static str> {
        println!("[netd] virtio-net bring-up (dev cap handle {})", DEV_CAP);

        // ── 1. Discover modern virtio-pci config structures ──────────────────
        let lay = virtio_pci::discover(DEV_CAP);
        if !lay.is_modern() {
            return Err("device does not expose modern virtio-pci capabilities");
        }

        // ── 2. Map the BAR(s) uncacheable ────────────────────────────────────
        let mut mapped = [0u64; 6];
        let common = Mmio::new(map_bar(lay.common.bar, &mut mapped)? + lay.common.offset as u64);
        let isr = Mmio::new(map_bar(lay.isr.bar, &mut mapped)? + lay.isr.offset as u64);
        let notify_base = map_bar(lay.notify.bar, &mut mapped)? + lay.notify.offset as u64;
        let device_cfg = if lay.device.present {
            Some(Mmio::new(
                map_bar(lay.device.bar, &mut mapped)? + lay.device.offset as u64,
            ))
        } else {
            None
        };

        // ── 3. Reset + feature negotiation ───────────────────────────────────
        common.write8(CFG_DEV_STATUS, 0);
        while common.read8(CFG_DEV_STATUS) != 0 {
            yield_now();
        }
        common.write8(CFG_DEV_STATUS, S_ACK);
        common.write8(CFG_DEV_STATUS, S_ACK | S_DRIVER);

        common.write32(CFG_DEV_FEAT_SEL, 1);
        let feat_hi = common.read32(CFG_DEV_FEAT);
        if feat_hi & F_VERSION_1_HI == 0 {
            return Err("device does not offer VIRTIO_F_VERSION_1");
        }
        // Accept only VERSION_1 — no offloads (checksums are computed in software).
        common.write32(CFG_DRV_FEAT_SEL, 0);
        common.write32(CFG_DRV_FEAT, 0);
        common.write32(CFG_DRV_FEAT_SEL, 1);
        common.write32(CFG_DRV_FEAT, F_VERSION_1_HI);

        common.write8(CFG_DEV_STATUS, S_ACK | S_DRIVER | S_FEATURES_OK);
        let st = common.read8(CFG_DEV_STATUS);
        if st & S_FEATURES_OK == 0 || st & S_FAILED != 0 {
            return Err("device rejected FEATURES_OK");
        }
        let _num_queues = common.read16(CFG_NUM_QUEUES);

        // ── 4. Set up RX + TX split virtqueues ───────────────────────────────
        let mut pool = DmaPool::new(DEV_CAP);
        let (mut rxq, rx_notify_off) = setup_queue(&common, &mut pool, RX_QUEUE)?;
        let (txq, tx_notify_off) = setup_queue(&common, &mut pool, TX_QUEUE)?;

        // Pre-post RX buffers: one WRITE descriptor per fixed buffer.
        let rx_pool = pool
            .alloc(NUM_RX as u64 * BUF_SIZE)
            .map_err(|_| "RX pool DMA alloc failed")?;
        let n_rx = NUM_RX.min(rxq.qsz);
        for i in 0..n_rx {
            let buf_phys = rx_pool.phys + i as u64 * BUF_SIZE;
            rxq.set_desc(i, buf_phys, BUF_SIZE as u32, VIRTQ_DESC_F_WRITE, 0);
            rxq.publish(i);
        }

        // TX buffers: one per descriptor slot, all initially free.
        let tx_pool = pool
            .alloc(NUM_TX as u64 * BUF_SIZE)
            .map_err(|_| "TX pool DMA alloc failed")?;
        let mut tx_free = [0u16; NUM_TX as usize];
        let n_tx = NUM_TX.min(txq.qsz);
        for i in 0..n_tx {
            tx_free[i as usize] = i;
        }

        // ── 5. DRIVER_OK — the device is live ────────────────────────────────
        common.write8(CFG_DEV_STATUS, S_ACK | S_DRIVER | S_FEATURES_OK | S_DRIVER_OK);

        let mut mac = [0u8; 6];
        if let Some(dc) = device_cfg {
            for (i, b) in mac.iter_mut().enumerate() {
                *b = dc.read8(i as u64);
            }
        }

        let nic = Nic {
            isr,
            notify_base,
            notify_mult: lay.notify_mult,
            rx_notify_off,
            tx_notify_off,
            rxq,
            txq,
            rx_pool,
            tx_pool,
            tx_free,
            tx_free_len: n_tx as usize,
            mac,
        };
        // Kick the device so it consumes the freshly posted RX buffers.
        nic.notify(RX_QUEUE, nic.rx_notify_off);
        println!(
            "[netd] virtqueues operational (RX {} buffers, TX {} slots) — DRIVER_OK",
            n_rx, n_tx
        );
        Ok(nic)
    }

    /// Notify the device that `queue` has new available buffers.
    fn notify(&self, queue: u16, notify_off: u16) {
        let addr = self.notify_base + notify_off as u64 * self.notify_mult as u64;
        unsafe { (addr as *mut u16).write_volatile(queue) };
    }

    /// Block until the device raises its IRQ (ring-3 IRQ path).
    pub fn wait_irq(&self) -> Result<(), &'static str> {
        sys_dev_irq_wait(DEV_CAP).map_err(|_| "SYS_DEV_IRQ_WAIT failed")
    }

    /// Read the ISR status to deassert the device INTx line, then ack/unmask.
    pub fn ack_irq(&self) {
        let _ = self.isr.read8(0);
        let _ = sys_dev_irq_ack(DEV_CAP);
    }

    /// Pop one received frame (net header stripped) into `out`, re-post its RX
    /// buffer, and return the frame length. `None` when the RX ring is drained.
    pub fn recv_into(&mut self, out: &mut [u8]) -> Option<usize> {
        let (id, used_len) = self.rxq.take_used()?;
        let idx = id as usize;
        let mut frame_len = 0usize;
        if (used_len as usize) > NET_HDR && idx < NUM_RX as usize {
            frame_len = used_len as usize - NET_HDR;
            let start = idx * BUF_SIZE as usize + NET_HDR;
            let n = frame_len.min(out.len());
            let src = &self.rx_pool.as_mut_slice()[start..start + n];
            out[..n].copy_from_slice(src);
            frame_len = n;
        }
        // Re-post the buffer (its descriptor still points at the same region) so
        // the device can reuse it, and notify the RX queue.
        if idx < NUM_RX as usize {
            self.rxq.publish(idx as u16);
            self.notify(RX_QUEUE, self.rx_notify_off);
        }
        Some(frame_len)
    }

    /// Reclaim any completed TX descriptors back onto the free list.
    fn reclaim_tx(&mut self) {
        while let Some((id, _len)) = self.txq.take_used() {
            if self.tx_free_len < self.tx_free.len() {
                self.tx_free[self.tx_free_len] = id as u16;
                self.tx_free_len += 1;
            }
        }
    }
}

impl FrameSink for Nic {
    /// Transmit one Ethernet frame: prepend the zeroed 12-byte virtio-net header,
    /// pad to the 60-byte Ethernet minimum, and publish a read-only descriptor.
    fn send_frame(&mut self, frame: &[u8]) {
        self.reclaim_tx();
        if self.tx_free_len == 0 {
            eprintln!("[netd] TX drop: no free TX buffer (device backlog)");
            return;
        }
        self.tx_free_len -= 1;
        let slot = self.tx_free[self.tx_free_len];

        let cap = BUF_SIZE as usize - NET_HDR;
        let payload = frame.len().min(cap);
        let padded = payload.max(crate::net::wire::ETH_MIN_FRAME).min(cap);

        let base = slot as usize * BUF_SIZE as usize;
        let buf = &mut self.tx_pool.as_mut_slice()[base..base + NET_HDR + padded];
        for b in buf.iter_mut() {
            *b = 0;
        }
        buf[NET_HDR..NET_HDR + payload].copy_from_slice(&frame[..payload]);

        let phys = self.tx_pool.phys + base as u64;
        self.txq.set_desc(slot, phys, (NET_HDR + padded) as u32, 0, 0);
        self.txq.publish(slot);
        self.notify(TX_QUEUE, self.tx_notify_off);
    }
}

/// Select, size, DMA-allocate, and enable one virtqueue. Returns the queue and
/// its `queue_notify_off`.
fn setup_queue(
    common: &Mmio,
    pool: &mut DmaPool,
    q: u16,
) -> Result<(SplitQueue, u16), &'static str> {
    common.write16(CFG_QUEUE_SEL, q);
    let dev_qsz = common.read16(CFG_QUEUE_SIZE);
    if dev_qsz == 0 {
        return Err("queue unavailable");
    }
    let qsz = dev_qsz.min(QDEPTH);
    common.write16(CFG_QUEUE_SIZE, qsz);

    let buf = pool
        .alloc(virtq::bytes(qsz))
        .map_err(|_| "virtqueue DMA alloc failed")?;
    let sq = SplitQueue::new(buf, qsz);

    common.write64(CFG_QUEUE_DESC, sq.desc_phys);
    common.write64(CFG_QUEUE_DRIVER, sq.avail_phys);
    common.write64(CFG_QUEUE_DEVICE, sq.used_phys);
    let notify_off = common.read16(CFG_QUEUE_NOTIFY_OFF);
    common.write16(CFG_QUEUE_ENABLE, 1);
    Ok((sq, notify_off))
}
