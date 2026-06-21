# Device drivers

## IOAPIC (`src/ioapic.rs`)

### Purpose

The IOAPIC routes hardware interrupts from devices to local APICs on each
CPU. Lythos replaces the legacy 8259 PIC (which is remapped to vectors
32–47 and then masked) with IOAPIC-based routing for VirtIO PCI interrupts.

### MMIO interface

The IOAPIC uses indirect register access through two MMIO registers:

| Offset | Register | Role |
|--------|----------|------|
| `+0x00` | IOREGSEL | Write the index of the register to access |
| `+0x10` | IOWIN | Read/write the selected register |

Standard register indices:

| Index | Name | Contents |
|-------|------|----------|
| `0x00` | IOAPICID | IOAPIC ID in bits [27:24] |
| `0x01` | IOAPICVER | Max redirection entry in bits [23:16] |
| `0x10 + gsi*2` | RTE_LO | Low 32 bits of redirection table entry |
| `0x11 + gsi*2` | RTE_HI | High 32 bits (destination APIC ID in bits [63:56]) |

### Redirection table entry (RTE) format

Each Global System Interrupt (GSI) has a 64-bit RTE:

| Bits | Field | Description |
|------|-------|-------------|
| 7:0 | Vector | IDT vector the interrupt delivers to |
| 10:8 | Delivery mode | 000 = Fixed (deliver to listed APIC) |
| 11 | Dest mode | 0 = Physical (APIC ID), 1 = Logical |
| 12 | Delivery status | Read-only; 1 = pending |
| 13 | Pin polarity | 0 = Active high, 1 = Active low |
| 14 | Remote IRR | Read-only; level-triggered state |
| 15 | Trigger mode | 0 = Edge, 1 = Level |
| 16 | Mask | 1 = Masked (interrupt suppressed) |
| 63:56 | Destination | Target APIC ID (physical mode) |

Lythos initialises all GSIs masked (`IRQ_MASKED = 1 << 16`). Individual
GSIs are unmasked as devices claim them.

### Init sequence (`ioapic::init`)

1. Map the IOAPIC MMIO page (`0xFEC0_0000` phys → `0xFFFF_8000_FEC0_0000`
   virt) with write-through, no-cache flags.
2. Read IOAPICVER to determine the number of redirection table entries
   (`MAX_ENTRY`).
3. Write all RTEs with `IRQ_MASKED` to ensure no spurious interrupts.

### API

| Function | Description |
|----------|-------------|
| `init()` | Map MMIO, read entry count, mask all GSIs |
| `map_irq(gsi, vector, flags)` | Program one RTE; deliver to BSP (APIC ID 0) |
| `mask_irq(gsi)` | Set the Mask bit in an RTE |
| `unmask_irq(gsi)` | Clear the Mask bit |
| `entry_count()` | Number of supported GSIs |
| `set_phys_base(phys)` | Override the IOAPIC physical base (for ACPI MADT) |

### ACPI integration note

The IOAPIC physical address is stored in an `AtomicU64` and defaults to
the QEMU standard `0xFEC0_0000`. When ACPI support is added, the MADT
parser should call `set_phys_base` before `init`.

---

## PCI scanner (`src/pci.rs`)

### Mechanism

Uses x86 Configuration Mechanism 1: write a 32-bit address to port `0xCF8`
(CONFIG_ADDRESS), then read/write port `0xCFC` (CONFIG_DATA) in 32-bit
chunks.

CONFIG_ADDRESS format:

```
Bit 31   : Enable bit (must be 1)
Bits 23:16: Bus number
Bits 15:11: Device number (0–31)
Bits 10:8 : Function number (0–7)
Bits 7:2  : Register offset (dword-aligned)
Bits 1:0  : Always 0
```

### Device discovery

`find_device(vendor_id, device_id)` scans bus 0, devices 0–31 (function 0
only — no multi-function device enumeration yet). For each device it reads
the vendor/device ID pair and returns a `PciDevice` struct on match.

`PciDevice` fields:

| Field | Source | Description |
|-------|--------|-------------|
| `bus`, `dev` | Scan loop | Location on the PCI bus |
| `vendor`, `device` | Config 0x00 | PCI IDs |
| `io_bar0` | Config 0x10 | I/O space BAR0 base address |
| `irq_line` | Config 0x3C | IRQ line (legacy; VirtIO uses this) |

After finding the device, `find_device` enables **bus mastering** (bit 2 of
the Command register at config offset 0x04). This is required for DMA.

---

## VirtIO block device (`src/virtio_blk.rs`)

### Overview

Implements the VirtIO 0.9 (legacy) block device specification using the
MMIO-over-PCI-I/O-BAR interface. PCI IDs: vendor `0x1AF4`, device `0x1001`.

All I/O is synchronous (polled). There is no interrupt-driven path yet,
though the device's IRQ line is read during init for future use.

### Port register map

All offsets relative to `io_base` (BAR0):

| Offset | Size | Name | Direction |
|--------|------|------|-----------|
| `0x00` | 4 | DEVICE_FEATURES | R |
| `0x04` | 4 | GUEST_FEATURES | W |
| `0x08` | 4 | QUEUE_PFN | R/W |
| `0x0C` | 2 | QUEUE_NUM | R |
| `0x0E` | 2 | QUEUE_SEL | W |
| `0x10` | 2 | QUEUE_NOTIFY | W |
| `0x12` | 1 | DEVICE_STATUS | R/W |
| `0x13` | 1 | ISR_STATUS | R/W (write-to-clear) |
| `0x14` | 4 | BLK_CAPACITY_LO | R |
| `0x18` | 4 | BLK_CAPACITY_HI | R |

### Device status sequence

```
DEVICE_STATUS = 0                    (reset)
DEVICE_STATUS = ACKNOWLEDGE (1)
DEVICE_STATUS = ACKNOWLEDGE | DRIVER (3)
  (negotiate features — none required for basic block I/O)
DEVICE_STATUS = ACKNOWLEDGE | DRIVER | DRIVER_OK (7)
```

### Virtqueue layout

The virtqueue uses `QUEUE_SIZE_MAX = 128` descriptors across two
physically-contiguous 4 KiB pages (`QUEUE_PAGES = 2`):

```
Page 0 (descriptors + available ring):
  [0x000..0x800]   128 × 16-byte descriptors
  [0x800..0xA02]   Available ring: flags(u16) + idx(u16) + ring[128](u16)

Page 1 (used ring):
  [0x000..0x408]   Used ring: flags(u16) + idx(u16) + ring[128](used_elem)
```

A `used_elem` is 8 bytes: `id(u32) + len(u32)`.

QUEUE_PFN is written as `vq_phys >> 12` (the page frame number).

### I/O operation: 3-descriptor chain

Each block read or write uses a chain of three descriptors:

```
Descriptor 0 — VirtIO block request header (16 bytes, device-readable)
  type (u32): 0 = read, 1 = write
  reserved (u32): 0
  sector (u64): target sector number

Descriptor 1 — Data buffer (512 bytes)
  Read:  device-writable (VRING_DESC_F_WRITE)
  Write: device-readable

Descriptor 2 — Status byte (1 byte, device-writable)
  0 = success, 1 = error, 2 = unsupported
```

Header and status are stored in two dedicated DMA frames (`hdr_phys`,
`dat_phys` — actually `dat_phys` holds the 512-byte sector buffer for
reads; writes supply the caller's data via a temporary mapping).

### Submit flow

1. Fill descriptor chain in the virtqueue.
2. `atomic::fence(SeqCst)` — ensure descriptor writes are visible before
   the kick.
3. Write `0` (queue index) to QUEUE_NOTIFY to kick the device.
4. Spin on `used_ring.idx` until it advances (polled completion).
5. Read and discard the ISR_STATUS register to acknowledge the interrupt
   (even in polled mode, clearing ISR_STATUS prevents spurious interrupts
   if interrupts are later enabled).

### Thread safety

`DEV` is a `DevState(UnsafeCell<Option<VirtioBlkDev>>)` with
`unsafe impl Sync`. Access is through `dev_mut()` / `dev_ref()` raw-pointer
helpers. This is safe as long as virtio operations are serialised
(single-CPU, cooperative scheduling guarantees this).
