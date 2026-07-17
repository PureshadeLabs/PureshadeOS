//! lythos-driver — userspace device-driver runtime.
//!
//! A ring-3 driver holds a `CapKind::Device` capability (handed to it by lythd)
//! and drives its device entirely from userspace over the framework syscalls in
//! `lythos-rt` (`sys_dev_*`). This crate wraps those into ergonomic pieces:
//!
//! * [`Mmio`] — a volatile MMIO register accessor over a mapped BAR,
//! * [`dma`] — a bump allocator over framework-minted DMA buffers,
//! * [`virtio_pci`] — modern virtio-pci capability discovery (walks config
//!   space via `sys_dev_cfg_read` — no port-I/O authority needed),
//! * [`virtq`] — split-virtqueue ring layout + producer/consumer helpers,
//!   the one piece that mirrors the kernel virtio ring math.
//!
//! The virtqueue ring layout is transport-agnostic (identical for legacy and
//! modern virtio); only discovery/notification differ, so this crate implements
//! the modern MMIO transport while sharing the ring structures conceptually
//! with the in-kernel legacy virtio-blk driver.

#![no_std]

pub mod mmio;
pub mod dma;
pub mod virtio_pci;
pub mod virtq;

pub use mmio::Mmio;
