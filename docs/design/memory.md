# Memory management

Lythos has three memory subsystems that build on each other:

```
heap (GlobalAlloc)
  └── vmm (4-level paging)
        └── pmm (frame bitmap)
```

---

## Physical Memory Manager (`src/pmm.rs`)

### Design

Bitmap allocator: one bit per 4 KiB frame. 4 GiB maximum addressable space
= 1 M frames = 128 KiB bitmap. Stored in a static `.bss` array
(`BITMAP: [u64; 16384]`).

Convention: **bit 0 = free, bit 1 = used.**

### Initialisation order

1. All frames marked **used** (bitmap all-ones).
2. Parse Multiboot1 or Multiboot2 memory map; mark type=1 (available) regions
   **free**.
3. Re-mark frames covered by the kernel image **used** (from linker symbols
   `KERNEL_START`, `KERNEL_END`).
4. Re-mark frame 0 **used** (BIOS data area / IVT).
5. Re-mark `0x400000..0x480000` **used** (lythd ELF blob; see
   [lythd.md](lythd.md)).
6. Count free frames; store in `FREE_FRAMES` atomic.

### API

| Function | Description |
|----------|-------------|
| `init(mb_magic, mb_info)` | One-shot init; must be called before any alloc |
| `alloc_frame()` | Allocate one 4 KiB frame; returns `Option<PhysAddr>` |
| `free_frame(addr)` | Return frame to pool; panics on double-free |
| `alloc_frames_contiguous(n)` | Allocate `n` physically-contiguous frames |
| `free_frames_contiguous(addr, n)` | Free `n` contiguous frames |
| `free_frame_count()` | Current free frame count |

### Thread safety

**Not locked.** The kernel is single-threaded (one CPU, cooperative scheduler).
A spinlock wrapper is required before SMP.

---

## Virtual Memory Manager (`src/vmm.rs`)

### Page table structure

Standard x86_64 4-level paging: PML4 → PDPT → PD → PT. Each level maps
9 bits of the virtual address. Leaf entries are 4 KiB pages (not huge).

Exception: the identity map in the first 1 GiB uses 2 MiB huge pages (PS=1
bit in PDE). The VMM does **not** manage those entries — they are built by
`boot.s` and must not be touched by `map_page`.

### Page flags

| Bit | Name | Meaning |
|-----|------|---------|
| 0 | Present | Entry valid |
| 1 | Write | Writable |
| 2 | User | Accessible from ring 3 |
| 63 | NX | No-execute (XD) |

The VMM accepts a `u64` flags argument. Common combinations:

| Flags | Use |
|-------|-----|
| `0x1` (P) | Read-only kernel page |
| `0x3` (P+W) | Read-write kernel page |
| `0x5` (P+U) | Read-only user page |
| `0x7` (P+W+U) | Read-write user page |
| `0x9` (P+NX) | No-exec kernel data |

### U/S propagation invariant

When `flags` includes the User bit (bit 2), `walk_or_create` sets bit 2 on
every intermediate table entry it touches. Forgetting this causes a
general-protection fault when user code accesses the page, because the CPU
checks U/S at every level.

### API

| Function | Description |
|----------|-------------|
| `init()` | Build full page table: identity map, higher-half, heap pre-map |
| `map_page(virt, phys, flags)` | Map one 4 KiB page |
| `unmap_page(virt)` | Unmap and flush TLB (`invlpg`) |
| `current_pml4_phys()` | Physical address of active PML4 |

### Do not call `map_page` on 0–1 GiB

Those addresses are covered by huge pages. `walk_or_create` will encounter a
PS=1 PDE and panic with "huge page encountered".

---

## Kernel Heap (`src/heap.rs`)

### Design

Linked-list allocator backed by the VMM. The heap region starts at
`0xFFFF_C000_0000_0000` and is pre-mapped for `HEAP_INIT_PAGES` pages at
`vmm::init` time. When the allocator needs more memory it calls `vmm::map_page`
+ `pmm::alloc_frame` to extend the heap on demand.

The allocator implements `core::alloc::GlobalAlloc` and is registered with
`#[global_allocator]`. After `heap::init()`, all Rust `alloc` crate types
(`Box`, `Vec`, `String`, etc.) are available.

### Heap region

| Field | Value |
|-------|-------|
| Base | `0xFFFF_C000_0000_0000` |
| Max size | 64 MiB |
| Pre-mapped | `HEAP_INIT_PAGES` × 4 KiB at boot |

### Allocation alignment

Minimum alignment is 8 bytes. The linked-list allocator stores a header
before each allocation and searches the free list first-fit.

### Locking

The heap allocator is protected by `serial::SpinLock`, which disables
interrupts on lock. This means heap allocations are safe from interrupt
context but must be brief.
