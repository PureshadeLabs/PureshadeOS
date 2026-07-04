use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use crate::{pmm, pci, vmm};

// Kernel VA for the linear framebuffer mapping (above IPC window at 0xFFFF_D000_...)
const FB_VA_BASE: u64 = 0xFFFF_E000_0000_0000;

static FB_VIRT:   AtomicU64 = AtomicU64::new(0);
static FB_PITCH:  AtomicU32 = AtomicU32::new(0);
static FB_WIDTH:  AtomicU32 = AtomicU32::new(0);
static FB_HEIGHT: AtomicU32 = AtomicU32::new(0);

// ── Bochs VBE (QEMU stdvga PCI 0x1234:0x1111) ────────────────────────────────
// Kept as a fallback for `-vga std` QEMU sessions where Limine may not supply
// a framebuffer response (e.g., headless BIOS-mode runs).

const VBE_INDEX: u16 = 0x01CE;
const VBE_DATA:  u16 = 0x01CF;

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

fn vbe_write(index: u16, value: u16) {
    unsafe { outw(VBE_INDEX, index); outw(VBE_DATA, value); }
}

/// Probe for QEMU stdvga, program 1024×768×32 via Bochs VBE I/O ports,
/// and return `(phys_addr, pitch, width, height, bpp)`.
fn init_bochs_vbe() -> Option<(u64, u32, u32, u32, u8)> {
    let (bus, dev) = pci::find(0x1234, 0x1111)?;
    pci::enable_io_mem(bus, dev);

    // BAR0 = linear framebuffer MMIO (memory space, bit 0 = 0).
    let bar0 = pci::read_bar32(bus, dev, 0);
    if bar0 & 1 != 0 { return None; } // unexpected I/O BAR
    let fb_phys = (bar0 & !0xF) as u64;
    if fb_phys == 0 { return None; }

    // Program Bochs VBE for 1024×768×32.
    vbe_write(4, 0);        // ENABLE = 0 (disable before changing mode)
    vbe_write(1, 1024);     // XRES
    vbe_write(2, 768);      // YRES
    vbe_write(3, 32);       // BPP
    vbe_write(6, 1024);     // VIRT_WIDTH  (= phys width, no scrolling)
    vbe_write(7, 768);      // VIRT_HEIGHT
    vbe_write(8, 0);        // X_OFFSET
    vbe_write(9, 0);        // Y_OFFSET
    vbe_write(4, 0x41);     // ENABLE = 1 | LFB (0x40)

    Some((fb_phys, 1024 * 4, 1024, 768, 32))
}

// ── Mapping helper ────────────────────────────────────────────────────────────

fn map_and_store(phys_addr: u64, pitch: u32, width: u32, height: u32) {
    let fb_bytes = pitch as u64 * height as u64;
    let pages = (fb_bytes + 4095) / 4096;

    // Map framebuffer physical pages into kernel VA.
    // PCD (bit 4) disables caching — required for MMIO/device memory.
    let flags = vmm::PageFlags(vmm::PageFlags::KERNEL_RW.0 | (1 << 4));
    for i in 0..pages {
        vmm::map_page(
            vmm::VirtAddr(FB_VA_BASE + i * 4096),
            pmm::PhysAddr(phys_addr + i * 4096),
            flags,
        );
    }

    FB_PITCH.store(pitch,   Ordering::Relaxed);
    FB_WIDTH.store(width,   Ordering::Relaxed);
    FB_HEIGHT.store(height, Ordering::Relaxed);
    // Release: makes the three stores visible before the virt pointer is published.
    FB_VIRT.store(FB_VA_BASE, Ordering::Release);
}

// ── Public init API ───────────────────────────────────────────────────────────

/// Initialise the framebuffer from a Limine framebuffer response.
///
/// `phys` is the physical address of the framebuffer MMIO region, recovered by
/// subtracting the HHDM offset from Limine's virtual `address` field before
/// `vmm::init()` discarded the Limine page tables.
///
/// Falls back to the Bochs VBE probe if `phys` is zero (no Limine response).
/// Returns `true` if a framebuffer was successfully mapped.
///
/// Must be called after `vmm::init()`.
pub fn init_from_limine(phys: u64, pitch: u64, width: u64, height: u64, bpp: u16) -> bool {
    let (phys, pitch, width, height, bpp) = if phys != 0 {
        (phys, pitch as u32, width as u32, height as u32, bpp as u8)
    } else {
        match init_bochs_vbe() {
            Some((p, pi, w, h, b)) => (p, pi, w, h, b),
            None => return false,
        }
    };

    if bpp != 32 || width == 0 || height == 0 { return false; }

    map_and_store(phys, pitch, width, height);
    true
}

// ── Public API ────────────────────────────────────────────────────────────────

pub fn is_active() -> bool { FB_VIRT.load(Ordering::Relaxed) != 0 }

/// Raw framebuffer parameters for the console: `(virt, pitch, width, height)`.
/// `None` until a framebuffer has been mapped.
pub(crate) fn raw() -> Option<(u64, u64, u32, u32)> {
    // Acquire pairs with the Release store in map_and_store: once the virt
    // pointer is visible, pitch/width/height are too.
    let virt = FB_VIRT.load(Ordering::Acquire);
    if virt == 0 { return None; }
    Some((
        virt,
        FB_PITCH.load(Ordering::Relaxed) as u64,
        FB_WIDTH.load(Ordering::Relaxed),
        FB_HEIGHT.load(Ordering::Relaxed),
    ))
}
pub fn width()  -> u32 { FB_WIDTH.load(Ordering::Relaxed) }
pub fn height() -> u32 { FB_HEIGHT.load(Ordering::Relaxed) }
pub fn dimensions() -> (u32, u32) { (width(), height()) }

/// Write a single pixel.  `rgb` = 0x00RRGGBB.
#[inline]
pub fn put_pixel(x: u32, y: u32, rgb: u32) {
    let virt = FB_VIRT.load(Ordering::Relaxed);
    if virt == 0 { return; }
    if x >= FB_WIDTH.load(Ordering::Relaxed) || y >= FB_HEIGHT.load(Ordering::Relaxed) { return; }
    let off = y as u64 * FB_PITCH.load(Ordering::Relaxed) as u64 + x as u64 * 4;
    unsafe { ((virt + off) as *mut u32).write_volatile(rgb & 0x00FF_FFFF); }
}

/// Fill a rectangle with a solid colour.  `rgb` = 0x00RRGGBB.
pub fn fill_rect(x: u32, y: u32, w: u32, h: u32, rgb: u32) {
    let virt = FB_VIRT.load(Ordering::Relaxed);
    if virt == 0 || w == 0 || h == 0 { return; }

    let pitch = FB_PITCH.load(Ordering::Relaxed) as u64;
    let fw    = FB_WIDTH.load(Ordering::Relaxed);
    let fh    = FB_HEIGHT.load(Ordering::Relaxed);

    let x1 = x.min(fw);
    let y1 = y.min(fh);
    let x2 = (x + w).min(fw);
    let y2 = (y + h).min(fh);
    if x2 <= x1 || y2 <= y1 { return; }

    let color = rgb & 0x00FF_FFFF;
    let span  = (x2 - x1) as u64;

    for py in y1..y2 {
        let row = virt + py as u64 * pitch + x1 as u64 * 4;
        for px in 0..span {
            unsafe { ((row + px * 4) as *mut u32).write_volatile(color); }
        }
    }
}

/// Clear the entire framebuffer to a solid colour.
pub fn clear(rgb: u32) {
    fill_rect(0, 0, width(), height(), rgb);
}

/// Draw a boot splash using simple geometric shapes.
/// No font required — purely graphical.
pub fn draw_splash() {
    let w = width();
    let h = height();
    if w == 0 || h == 0 { return; }

    // Background
    clear(0x0D1117);

    // 6-px accent bar split into 4 colour bands
    let bands: [u32; 4] = [0x6366F1, 0x8B5CF6, 0x06B6D4, 0x10B981];
    let bw = w / 4;
    for (i, &c) in bands.iter().enumerate() {
        fill_rect(i as u32 * bw, 0, bw, 6, c);
    }
    // Fill any remainder pixel from integer division
    fill_rect(bw * 4, 0, w - bw * 4, 6, bands[3]);

    // Subtle horizontal rule below accent
    fill_rect(0, 6, w, 1, 0x21262D);

    // Bottom status bar
    fill_rect(0, h - 1, w, 1, 0x21262D);
    fill_rect(0, h - 24, w, 23, 0x161B22);

    // Centre panel (represents logo / info area in future)
    let px = w / 2 - 120;
    let py = h / 2 - 40;
    fill_rect(px,     py,     240, 80, 0x21262D);  // border
    fill_rect(px + 2, py + 2, 236, 76, 0x0D1117);  // inner
}
