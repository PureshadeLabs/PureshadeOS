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
    /// Kernel read-write-execute (used for the initial kernel image mapping;
    /// per-section RX/RW split is deferred until linker section symbols are
    /// wired through vmm::init()).
    pub const KERNEL_RWX: Self = Self(
        PageFlags::PRESENT.0 | PageFlags::WRITABLE.0
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

/// Offset added to a physical address to reach a virtual mapping of it.
///
/// Limine base revision ≥ 1 does NOT identity-map lower memory — under the
/// boot page tables, frames are only reachable through the HHDM.  `init`
/// sets this to the HHDM offset before the first `alloc_table()` call and
/// leaves it set afterwards: step 3.5 reproduces the HHDM alias in our own
/// tables, so the same offset stays valid after the CR3 switch.
static mut PHYS_OFFSET: u64 = 0;

// ── Internal helpers ──────────────────────────────────────────────────────────

/// Virtual pointer to the page-table frame at `phys` (via HHDM offset).
#[inline]
fn table_ptr(phys: PhysAddr) -> *mut PageTable {
    (phys.as_u64() + unsafe { PHYS_OFFSET }) as *mut PageTable
}

/// Allocate a zeroed 4 KiB frame for a page table.
fn alloc_table() -> PhysAddr {
    let frame = pmm::alloc_frame().expect("vmm: out of memory for page table");
    unsafe { core::ptr::write_bytes(table_ptr(frame) as *mut u8, 0, 4096) };
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
            unsafe { &mut *table_ptr(entry.address()) }
        }};
    }
    let p4 = unsafe { &mut *table_ptr(pml4) };
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
            unsafe { &mut *table_ptr(entry.address()) }
        }};
    }
    let p4 = unsafe { &mut *table_ptr(pml4) };
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

/// Remove the mapping for `virt` in `pml4`, returning the physical frame that
/// backed it (or `None` if it was not mapped).  The caller decides whether to
/// free the frame.  Invalidates the TLB entry — `pml4` may be the active CR3
/// (SYS_BRK shrink runs on the calling task's own page table).
pub fn unmap_page_in(pml4: PhysAddr, virt: VirtAddr) -> Option<PhysAddr> {
    let entry = unsafe { walk_existing(pml4, virt) }?;
    if !entry.is_present() { return None; }
    let pa = entry.address();
    entry.clear();
    unsafe {
        core::arch::asm!(
            "invlpg [{va}]",
            va = in(reg) virt.as_u64(),
            options(nostack, preserves_flags),
        );
    }
    crate::apic::send_tlb_shootdown_ipi();
    Some(pa)
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
    let kern_p4     = unsafe { &*table_ptr(PhysAddr(PML4_PHYS)) };
    let kern_p3     = unsafe { &*table_ptr(kern_p4.0[0].address()) };
    let shared_p2   = kern_p3.0[0].address();

    let p4 = unsafe { &*table_ptr(pml4) };

    // Walk user-half PML4 entries only.
    for i in 0..256 {
        let p4e = p4.0[i];
        if !p4e.is_present() { continue; }
        let p3_phys = p4e.address();
        let p3      = unsafe { &*table_ptr(p3_phys) };

        for j in 0..512 {
            let p3e = p3.0[j];
            if !p3e.is_present() { continue; }
            let p2_phys = p3e.address();

            // Skip the shared identity-map PD.
            if p2_phys == shared_p2 { continue; }

            let p2 = unsafe { &*table_ptr(p2_phys) };

            for k in 0..512 {
                let p2e = p2.0[k];
                if !p2e.is_present() { continue; }
                if p2e.is_huge() {
                    // 2 MiB page — free the backing frame directly.
                    pmm::free_frame(p2e.address());
                    continue;
                }
                let p1_phys = p2e.address();
                let p1      = unsafe { &*table_ptr(p1_phys) };
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
    let kern_p4  = unsafe { &*table_ptr(PhysAddr(PML4_PHYS)) };
    let p3_phys  = kern_p4.0[0].address();
    let kern_p3  = unsafe { &*table_ptr(p3_phys) };
    let p2_phys  = kern_p3.0[0].address();

    // New per-process PDPT: slot 0 → shared identity-map PD (no U/S —
    // kernel identity map is not user-accessible).
    let user_p3_phys = alloc_table();
    let user_p3      = unsafe { &mut *table_ptr(user_p3_phys) };
    user_p3.0[0]     = PageTableEntry::table(p2_phys);

    // New PML4.
    let new_pml4_phys = alloc_table();
    let new_p4        = unsafe { &mut *table_ptr(new_pml4_phys) };

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

/// Initialise the VMM.
///
/// `kernel_phys_base` / `kernel_virt_base` come from Limine's
/// KernelAddressResponse.  `kernel_phys_end` is the physical end of the
/// kernel image (computed in kmain from KERNEL_END − KERNEL_START + phys_base).
/// `hhdm_off` is Limine's HHDM offset (HhdmResponse.offset).  Limine places
/// the initial stack in the HHDM range; we re-map 0→1 GiB there so the stack
/// and other Limine-allocated structures remain accessible after our CR3 is loaded.
///
/// Must be called once, after `pmm::init()`, before any `map_page` calls.
macro_rules! dbg_byte {
    ($b:expr) => {
        unsafe {
            core::arch::asm!(
                "mov dx, 0x3F8", "out dx, al",
                in("al") $b as u8, out("dx") _, options(nostack, nomem, preserves_flags)
            );
        }
    };
}

pub fn init(kernel_phys_base: u64, kernel_virt_base: u64, kernel_phys_end: u64, hhdm_off: u64) {
    dbg_byte!(b'1');  // entered init
    // Limine base revision ≥ 1 provides no identity map of lower memory —
    // page-table frames are only reachable through the HHDM.  Must be set
    // before the first alloc_table() below.
    unsafe { PHYS_OFFSET = hhdm_off; }
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

    dbg_byte!(b'2');  // NXE done
    // ── 2. Allocate PML4 and record it ────────────────────────────────────
    // Table frames come from the PMM while Limine's page tables are still in
    // CR3; they are written through the HHDM (PHYS_OFFSET above).
    let pml4_phys = alloc_table();
    unsafe { PML4_PHYS = pml4_phys.as_u64(); }

    dbg_byte!(b'3');  // PML4 allocated
    // ── 3. Identity-map 0→1 GiB with 2 MiB huge pages ────────────────────
    // Only three frames needed (PML4, PDPT, PD); no PT level.
    // Flags: present (bit 0) | writable (bit 1) | PS (bit 7) = 0x83.
    // NX is intentionally NOT set: kernel code executes from this range.
    // p2_phys is kept for the HHDM alias in step 3.5.
    let p2_phys = {
        let p3_phys = alloc_table();
        let p2_phys = alloc_table();

        // PML4[0] → PDPT
        let p4 = table_ptr(pml4_phys);
        unsafe { (*p4).0[0] = PageTableEntry::table(p3_phys); }

        // PDPT[0] → PD
        let p3 = table_ptr(p3_phys);
        unsafe { (*p3).0[0] = PageTableEntry::table(p2_phys); }

        // PD[0..512]: 2 MiB huge pages, physical = i × 2 MiB
        let p2 = table_ptr(p2_phys);
        for i in 0..512_usize {
            let base = (i as u64) * 2 * 1024 * 1024;
            unsafe { (*p2).0[i] = PageTableEntry(base | 0x83); }
        }
        p2_phys
    };

    dbg_byte!(b'4');  // identity map done
    // ── 3.5: HHDM alias — re-map 0→1 GiB at hhdm_off ────────────────────
    // Limine places the boot stack and other runtime structures at
    // `hhdm_off + phys`.  Without this alias those addresses are unmapped
    // in our new CR3, causing an immediate stack-access fault on the first
    // `ret` after `mov cr3`.
    // We share the same PD (p2_phys) already set up in step 3.
    {
        let hhdm_p4_idx = ((hhdm_off >> 39) & 0x1FF) as usize;
        // Only needed when HHDM base is in a different PML4 slot than identity.
        if hhdm_p4_idx != 0 {
            let hhdm_p3_phys = alloc_table();
            let hhdm_p3 = table_ptr(hhdm_p3_phys);
            // hhdm_off is always 1-GiB aligned (Limine guarantee),
            // so PDPT index 0 covers the full first GiB of HHDM.
            unsafe { (*hhdm_p3).0[0] = PageTableEntry::table(p2_phys); }
            let p4 = table_ptr(pml4_phys);
            unsafe { (*p4).0[hhdm_p4_idx] = PageTableEntry::table(hhdm_p3_phys); }
        }
    }

    dbg_byte!(b'5');  // HHDM alias done
    // ── 4. Map kernel at its higher-half virtual address ──────────────────
    // The kernel is linked at 0xFFFFFFFF80100000+ (higher half, Limine v5+
    // requirement).  Limine placed the kernel at kernel_phys_base physically
    // and mapped it to kernel_virt_base virtually.  We must reproduce that
    // mapping in our new PML4 so execution continues after CR3 is loaded.
    {
        let kernel_phys_end_aligned = (kernel_phys_end + FRAME_SIZE - 1) & !(FRAME_SIZE - 1);
        let mut pa = kernel_phys_base;
        let mut va = kernel_virt_base;
        while pa < kernel_phys_end_aligned {
            map_page(VirtAddr(va), PhysAddr(pa), PageFlags::KERNEL_RWX);
            pa += FRAME_SIZE;
            va += FRAME_SIZE;
        }
    }

    dbg_byte!(b'6');  // kernel mapped
    // ── 5. Load new CR3 ───────────────────────────────────────────────────
    // Flushes the TLB; execution continues from the kernel higher-half mapping.
    // The HHDM alias (step 3.5) keeps the Limine boot stack accessible.
    unsafe {
        core::arch::asm!(
            // Debug: write 'P' to COM1 (0x3F8) just before CR3 swap
            "mov dx, 0x3F8",
            "mov al, 0x50",  // 'P'
            "out dx, al",
            "mov cr3, {pml4}",
            // Debug: write 'Q' to COM1 just after CR3 swap (stack must be accessible)
            "mov dx, 0x3F8",
            "mov al, 0x51",  // 'Q'
            "out dx, al",
            pml4 = in(reg) pml4_phys.as_u64(),
            options(nostack),
            out("eax") _,
            out("edx") _,
        );
    }
}
