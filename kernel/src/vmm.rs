/// Virtual Memory Manager — 4-level paging (PML4 → PDPT → PD → PT), 4 KiB pages.
///
/// ## Identity map strategy (2 MiB huge pages)
///
/// The entire first 1 GiB is identity-mapped using 2 MiB huge pages in the PD
/// level (PS=1), requiring only three page-table frames (PML4, PDPT, PD).
/// This ensures every physical frame the PMM can ever hand out (the QEMU
/// default is 128 MiB) is always reachable as phys == virt, so page-table
/// frames and heap backing frames are accessible after CR3 is loaded.
///
/// **No NX on the identity map**: the kernel executes from physical addresses
/// in this range.  Finer code/data separation is deferred.
///
/// ## Higher-half window
///
/// The kernel image is additionally mapped at `0xFFFF_8000_0000_0000 + pa`
/// with 4 KiB pages and NX (data-only; execution from higher-half is
/// deferred to a later step).
///
/// ## `map_page` / `unmap_page`
///
/// Operate at 4 KiB granularity on *any* virtual address **outside** the
/// 0→1 GiB identity range.  Calling them on identity-mapped addresses will
/// panic because `walk_or_create` detects the PS=1 huge page and refuses to
/// split it.

use crate::pmm::{self, PhysAddr, FRAME_SIZE};

// ── VirtAddr ──────────────────────────────────────────────────────────────────

/// A 64-bit virtual address.  Newtype prevents confusion with `PhysAddr`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct VirtAddr(pub u64);

impl VirtAddr {
    #[inline] pub fn as_u64(self) -> u64 { self.0 }

    #[inline] fn p4_idx(self) -> usize { ((self.0 >> 39) & 0x1FF) as usize }
    #[inline] fn p3_idx(self) -> usize { ((self.0 >> 30) & 0x1FF) as usize }
    #[inline] fn p2_idx(self) -> usize { ((self.0 >> 21) & 0x1FF) as usize }
    #[inline] fn p1_idx(self) -> usize { ((self.0 >> 12) & 0x1FF) as usize }
}

// ── PageFlags ─────────────────────────────────────────────────────────────────

/// x86-64 page-table entry flag bits.
#[derive(Clone, Copy, Debug)]
pub struct PageFlags(pub u64);

impl PageFlags {
    pub const PRESENT:  Self = Self(1 << 0);
    pub const WRITABLE: Self = Self(1 << 1);
    pub const USER:     Self = Self(1 << 2);
    pub const NX:       Self = Self(1 << 63);

    /// Kernel read-write, no-execute (heap, data, stack pages).
    pub const KERNEL_RW: Self = Self(
        PageFlags::PRESENT.0 | PageFlags::WRITABLE.0 | PageFlags::NX.0
    );
    /// Kernel read-only, no-execute.
    pub const KERNEL_RO: Self = Self(
        PageFlags::PRESENT.0 | PageFlags::NX.0
    );
    /// User read-execute (code pages; not executable from kernel mode with SMEP).
    pub const USER_RX: Self = Self(
        PageFlags::PRESENT.0 | PageFlags::USER.0
    );
    /// User read-write, no-execute (stack and data pages).
    pub const USER_RW: Self = Self(
        PageFlags::PRESENT.0 | PageFlags::WRITABLE.0 | PageFlags::USER.0 | PageFlags::NX.0
    );
}

impl core::ops::BitOr for PageFlags {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self { Self(self.0 | rhs.0) }
}

// ── PageTableEntry ────────────────────────────────────────────────────────────

/// Mask extracting the physical frame address from a PTE (bits 51:12).
const PHYS_ADDR_MASK: u64 = 0x000F_FFFF_FFFF_F000;

/// A single 8-byte page-table entry.
#[repr(transparent)]
#[derive(Clone, Copy)]
pub struct PageTableEntry(u64);

impl PageTableEntry {
    pub const fn zero() -> Self { Self(0) }

    #[inline] pub fn is_present(self) -> bool { self.0 & 1 != 0 }
    /// True when the PS (Page Size) bit is set — indicates a huge page at
    /// the PD or PDPT level rather than a pointer to the next table level.
    #[inline] fn is_huge(self)    -> bool { self.0 & (1 << 7) != 0 }

    /// Set the entry to map `phys` with the given `flags`.
    #[inline]
    pub fn set(&mut self, phys: PhysAddr, flags: PageFlags) {
        self.0 = (phys.as_u64() & PHYS_ADDR_MASK) | flags.0;
    }

    /// Clear the entry (mark not-present).
    #[inline]
    pub fn clear(&mut self) { self.0 = 0; }

    /// Extract the physical address stored in this entry.
    #[inline]
    pub fn address(self) -> PhysAddr { PhysAddr(self.0 & PHYS_ADDR_MASK) }

    /// Build an intermediate table entry: present + writable, no NX.
    #[inline]
    fn table(phys: PhysAddr) -> Self {
        Self(phys.as_u64() | 0x3) // present | writable
    }
}

// ── PageTable ─────────────────────────────────────────────────────────────────

/// A 4 KiB page table — 512 × 8-byte entries.
#[repr(C, align(4096))]
struct PageTable([PageTableEntry; 512]);

// ── VMM state ─────────────────────────────────────────────────────────────────

/// Physical address of the active PML4.
static mut PML4_PHYS: u64 = 0;

// ── Internal helpers ──────────────────────────────────────────────────────────

/// Allocate a zeroed 4 KiB frame for a page table.
fn alloc_table() -> PhysAddr {
    let frame = pmm::alloc_frame().expect("vmm: out of memory for page table");
    // All PMM frames are within the identity-mapped 0→1 GiB range, so phys == virt.
    unsafe { core::ptr::write_bytes(frame.as_u64() as *mut u8, 0, 4096) };
    frame
}

/// Walk the P4 → P3 → P2 → P1 chain for `virt`, creating intermediate tables
/// as needed.  Returns a mutable reference to the leaf P1 entry.
///
/// `user`: if true, every intermediate entry traversed will have the U/S bit
/// (bit 2) set.  x86_64 requires U/S=1 at *every* level for CPL=3 accesses
/// to succeed; kernel-only mappings leave U/S clear on intermediate entries.
///
/// # Panics
/// Panics if any intermediate PD entry is a 2 MiB huge page (PS=1).
/// Callers must not use this for virtual addresses in the 0→1 GiB identity
/// range, which is mapped with huge pages.
unsafe fn walk_or_create(pml4: PhysAddr, virt: VirtAddr, user: bool) -> &'static mut PageTableEntry {
    macro_rules! descend {
        ($parent:expr, $idx:expr) => {{
            let entry = &mut $parent.0[$idx];
            if !entry.is_present() {
                *entry = PageTableEntry::table(alloc_table());
            }
            // For user mappings, all intermediate entries must have U/S set.
            if user { entry.0 |= 1 << 2; }
            assert!(
                !entry.is_huge(),
                "vmm: map_page hit a huge-page entry — \
                 do not call map_page for addresses in the 0→1 GiB identity range"
            );
            unsafe { &mut *(entry.address().as_u64() as *mut PageTable) }
        }};
    }
    let p4 = unsafe { &mut *(pml4.as_u64() as *mut PageTable) };
    let p3 = descend!(p4, virt.p4_idx());
    let p2 = descend!(p3, virt.p3_idx());
    let p1 = descend!(p2, virt.p2_idx());
    &mut p1.0[virt.p1_idx()]
}

/// Walk without creating.  Returns `None` if any level is not-present.
unsafe fn walk_existing(pml4: PhysAddr, virt: VirtAddr) -> Option<&'static mut PageTableEntry> {
    macro_rules! descend {
        ($parent:expr, $idx:expr) => {{
            let entry = &$parent.0[$idx];
            if !entry.is_present() { return None; }
            unsafe { &mut *(entry.address().as_u64() as *mut PageTable) }
        }};
    }
    let p4 = unsafe { &mut *(pml4.as_u64() as *mut PageTable) };
    let p3 = descend!(p4, virt.p4_idx());
    let p2 = descend!(p3, virt.p3_idx());
    let p1 = descend!(p2, virt.p2_idx());
    Some(&mut p1.0[virt.p1_idx()])
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Map `virt` → `phys` with `flags` in the active page tables.
///
/// Intermediate page tables are allocated from the PMM as needed.
/// Invalidates the TLB entry for `virt`.
///
/// # Panics
/// Panics if `virt` falls inside the 0→1 GiB identity range (huge pages).
pub fn map_page(virt: VirtAddr, phys: PhysAddr, flags: PageFlags) {
    let pml4 = PhysAddr(unsafe { PML4_PHYS });
    let user = flags.0 & (1 << 2) != 0; // propagate U/S through intermediate entries
    let entry = unsafe { walk_or_create(pml4, virt, user) };
    entry.set(phys, flags);
    unsafe {
        core::arch::asm!(
            "invlpg [{va}]",
            va = in(reg) virt.as_u64(),
            options(nostack, preserves_flags),
        );
    }
}

/// Return the physical address currently mapped at `virt`, or `None` if the
/// page is not present at any level of the table walk.
pub fn query_page(virt: VirtAddr) -> Option<PhysAddr> {
    let pml4 = PhysAddr(unsafe { PML4_PHYS });
    unsafe { walk_existing(pml4, virt) }
        .filter(|e| e.is_present())
        .map(|e| e.address())
}

/// Update the flags on an already-mapped page without changing the physical
/// frame it points to.  No-op if the page is not present.
pub fn update_page_flags(virt: VirtAddr, flags: PageFlags) {
    let pml4 = PhysAddr(unsafe { PML4_PHYS });
    if let Some(entry) = unsafe { walk_existing(pml4, virt) } {
        if entry.is_present() {
            let phys = entry.address();
            entry.set(phys, flags);
            unsafe {
                core::arch::asm!(
                    "invlpg [{va}]",
                    va = in(reg) virt.as_u64(),
                    options(nostack, preserves_flags),
                );
            }
        }
    }
}

/// Remove the mapping for `virt`.  No-op if the address was not mapped.
/// Invalidates the local TLB entry and sends a shootdown IPI to all other
/// CPUs so they flush stale entries for this address.
pub fn unmap_page(virt: VirtAddr) {
    let pml4 = PhysAddr(unsafe { PML4_PHYS });
    if let Some(entry) = unsafe { walk_existing(pml4, virt) } {
        entry.clear();
    }
    unsafe {
        core::arch::asm!(
            "invlpg [{va}]",
            va = in(reg) virt.as_u64(),
            options(nostack, preserves_flags),
        );
    }
    crate::apic::send_tlb_shootdown_ipi();
}

/// Free all frames owned by a user process's page table, then free the table
/// frames themselves.
///
/// Walks PML4[0..255] only (the kernel higher-half [256..511] is shared and
/// must not be touched).  Within PML4[0] the identity-map PD (shared across
/// all processes) is identified and skipped — only per-process intermediate
/// tables and leaf pages are freed.
///
/// # Safety
/// `pml4` must be the physical address of a page table created by
/// `create_user_page_table` and must not be the currently active CR3.
pub fn free_user_page_table(pml4: PhysAddr) {
    // Locate the shared identity-map PD so we can skip it.
    let kern_p4     = unsafe { &*(PML4_PHYS as *const PageTable) };
    let kern_p3     = unsafe { &*(kern_p4.0[0].address().as_u64() as *const PageTable) };
    let shared_p2   = kern_p3.0[0].address();

    let p4 = unsafe { &*(pml4.as_u64() as *const PageTable) };

    // Walk user-half PML4 entries only.
    for i in 0..256 {
        let p4e = p4.0[i];
        if !p4e.is_present() { continue; }
        let p3_phys = p4e.address();
        let p3      = unsafe { &*(p3_phys.as_u64() as *const PageTable) };

        for j in 0..512 {
            let p3e = p3.0[j];
            if !p3e.is_present() { continue; }
            let p2_phys = p3e.address();

            // Skip the shared identity-map PD.
            if p2_phys == shared_p2 { continue; }

            let p2 = unsafe { &*(p2_phys.as_u64() as *const PageTable) };

            for k in 0..512 {
                let p2e = p2.0[k];
                if !p2e.is_present() { continue; }
                if p2e.is_huge() {
                    // 2 MiB page — free the backing frame directly.
                    pmm::free_frame(p2e.address());
                    continue;
                }
                let p1_phys = p2e.address();
                let p1      = unsafe { &*(p1_phys.as_u64() as *const PageTable) };
                for l in 0..512 {
                    let p1e = p1.0[l];
                    if p1e.is_present() {
                        pmm::free_frame(p1e.address()); // leaf page frame
                    }
                }
                pmm::free_frame(p1_phys); // PT frame
            }
            pmm::free_frame(p2_phys); // PD frame
        }
        pmm::free_frame(p3_phys); // PDPT frame
    }
    pmm::free_frame(pml4); // PML4 frame
}

/// Return the physical address of the kernel's PML4.
pub fn kernel_pml4() -> PhysAddr {
    PhysAddr(unsafe { PML4_PHYS })
}

/// Create a fresh user-process page table that shares the kernel mappings.
///
/// Layout of the new PML4:
/// - `[0]`       → fresh PDPT whose slot 0 re-uses the kernel's identity-map
///                 PD (2 MiB huge pages, 0→1 GiB).  The PML4 entry carries
///                 U/S so that `walk_or_create` can add per-process PDPT
///                 entries (slots 4+) for user segments above 1 GiB.
/// - `[256..511]` copied from the kernel PML4 (heap, IPC window, etc.).
pub fn create_user_page_table() -> PhysAddr {
    // Locate the kernel's identity-map PD by reading PML4[0] → PDPT[0].
    let kern_p4  = unsafe { &*(PML4_PHYS as *const PageTable) };
    let p3_phys  = kern_p4.0[0].address();
    let kern_p3  = unsafe { &*(p3_phys.as_u64() as *const PageTable) };
    let p2_phys  = kern_p3.0[0].address();

    // New per-process PDPT: slot 0 → shared identity-map PD (no U/S —
    // kernel identity map is not user-accessible).
    let user_p3_phys = alloc_table();
    let user_p3      = unsafe { &mut *(user_p3_phys.as_u64() as *mut PageTable) };
    user_p3.0[0]     = PageTableEntry::table(p2_phys);

    // New PML4.
    let new_pml4_phys = alloc_table();
    let new_p4        = unsafe { &mut *(new_pml4_phys.as_u64() as *mut PageTable) };

    // PML4[0] → user PDPT with U/S so walk_or_create can add user entries.
    new_p4.0[0] = PageTableEntry(user_p3_phys.as_u64() | 0x7); // P | W | U/S

    // Copy kernel higher-half mappings PML4[256..511].
    for i in 256..512 {
        new_p4.0[i] = kern_p4.0[i];
    }

    new_pml4_phys
}

/// Map `virt` → `phys` in `pml4` without issuing `invlpg`.
///
/// Use this when building page tables for tasks that are not currently
/// loaded in CR3.  For the active page table use `map_page`.
pub fn map_page_in(pml4: PhysAddr, virt: VirtAddr, phys: PhysAddr, flags: PageFlags) {
    let user  = flags.0 & (1 << 2) != 0;
    let entry = unsafe { walk_or_create(pml4, virt, user) };
    entry.set(phys, flags);
}

/// Query a page mapping in `pml4`.  Returns `None` if any table level is absent.
pub fn query_page_in(pml4: PhysAddr, virt: VirtAddr) -> Option<PhysAddr> {
    unsafe { walk_existing(pml4, virt) }
        .filter(|e| e.is_present())
        .map(|e| e.address())
}

/// Update flags on an existing mapping in `pml4`.  No-op if not present.
pub fn update_page_flags_in(pml4: PhysAddr, virt: VirtAddr, flags: PageFlags) {
    if let Some(entry) = unsafe { walk_existing(pml4, virt) } {
        if entry.is_present() {
            let phys = entry.address();
            entry.set(phys, flags);
        }
    }
}

/// Clear a page mapping in `pml4`.  No-op if not present.  No `invlpg`.
pub fn unmap_page_in(pml4: PhysAddr, virt: VirtAddr) {
    if let Some(entry) = unsafe { walk_existing(pml4, virt) } {
        entry.clear();
    }
}

/// Initialise the VMM.
///
/// Must be called once, after `pmm::init()`, before any `map_page` calls.
pub fn init() {
    // ── 1. Enable NXE in EFER ─────────────────────────────────────────────
    // IA32_EFER MSR = 0xC000_0080; NXE is bit 11.
    // Without NXE, bit 63 of a PTE is a reserved bit — setting it causes #PF.
    unsafe {
        let mut lo: u32;
        let mut hi: u32;
        core::arch::asm!(
            "rdmsr",
            in("ecx") 0xC000_0080u32,
            out("eax") lo,
            out("edx") hi,
            options(nostack, nomem),
        );
        lo |= 1 << 11; // set NXE
        core::arch::asm!(
            "wrmsr",
            in("ecx") 0xC000_0080u32,
            in("eax") lo,
            in("edx") hi,
            options(nostack, nomem),
        );
    }

    // ── 2. Allocate PML4 and record it ────────────────────────────────────
    // All table frames come from the PMM while the boot page tables (which
    // identity-map 0→1 GiB with 2 MiB huge pages) are still in CR3, so
    // every physical address is directly accessible.
    let pml4_phys = alloc_table();
    unsafe { PML4_PHYS = pml4_phys.as_u64(); }

    // ── 3. Identity-map 0→1 GiB with 2 MiB huge pages ────────────────────
    // Only three frames needed (PML4, PDPT, PD); no PT level.
    // Flags: present (bit 0) | writable (bit 1) | PS (bit 7) = 0x83.
    // NX is intentionally NOT set: kernel code executes from this range.
    {
        let p3_phys = alloc_table();
        let p2_phys = alloc_table();

        // PML4[0] → PDPT
        let p4 = pml4_phys.as_u64() as *mut PageTable;
        unsafe { (*p4).0[0] = PageTableEntry::table(p3_phys); }

        // PDPT[0] → PD
        let p3 = p3_phys.as_u64() as *mut PageTable;
        unsafe { (*p3).0[0] = PageTableEntry::table(p2_phys); }

        // PD[0..512]: 2 MiB huge pages, physical = i × 2 MiB
        let p2 = p2_phys.as_u64() as *mut PageTable;
        for i in 0..512_usize {
            let base = (i as u64) * 2 * 1024 * 1024;
            unsafe { (*p2).0[i] = PageTableEntry(base | 0x83); }
        }
    }

    // ── 4. Higher-half kernel window (4 KiB pages, NX) ───────────────────
    // Maps the kernel image at 0xFFFF_8000_0000_0000 + physical_address.
    // Execution from higher-half is deferred; NX is set (data mapping).
    unsafe extern "C" {
        static KERNEL_START: u8;
        static KERNEL_END:   u8;
    }
    let kstart = &raw const KERNEL_START as u64;
    let kend   = (&raw const KERNEL_END as u64 + FRAME_SIZE - 1) & !(FRAME_SIZE - 1);

    const HIGHER_HALF: u64 = 0xFFFF_8000_0000_0000;
    {
        let mut pa = kstart;
        while pa < kend {
            map_page(VirtAddr(HIGHER_HALF + pa), PhysAddr(pa), PageFlags::KERNEL_RW);
            pa += FRAME_SIZE;
        }
    }

    // ── 5. Load new CR3 ───────────────────────────────────────────────────
    // Flushes the TLB; execution continues from the identity-mapped
    // physical addresses (same mapping as before, now via huge pages).
    unsafe {
        core::arch::asm!(
            "mov cr3, {pml4}",
            pml4 = in(reg) pml4_phys.as_u64(),
            options(nostack),
        );
    }
}
