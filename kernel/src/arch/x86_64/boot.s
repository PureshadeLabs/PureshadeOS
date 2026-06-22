/*
 * boot.s — Lythos (Capability-Aware System Kernel) x86_64 boot stub
 *
 * Both Multiboot1 (QEMU -kernel / SeaBIOS) and Multiboot2 (GRUB2) headers
 * are present so the same ELF works in both environments.
 *
 * Both protocols enter in 32-bit protected mode (flat 4 GiB segments).
 * This stub then:
 *   1. Sets up a temporary 128 KiB stack
 *   2. Zeros the three page-table frames and identity-maps the first 1 GiB
 *      using 2 MiB huge pages (P4 → P3 → P2 with PS=1)
 *   3. Enables PAE, loads CR3, sets EFER.LME, enables paging
 *   4. Loads a minimal 64-bit GDT and far-jumps to activate long mode
 *   5. Reloads segment registers and calls kmain()
 */

/* ── Multiboot1 header (QEMU -kernel uses SeaBIOS Multiboot1) ──────────── */
/* Bit 16 (a.out kludge) tells QEMU to use our address fields directly      */
/* instead of parsing the ELF header.  This is required because QEMU's MB1  */
/* loader rejects 64-bit ELFs without the kludge.                           */
/* NOTE: with the a.out kludge, QEMU does NOT populate MB1 info flags or    */
/* the modules list.  lythd is therefore passed via -device loader (see     */
/* run.sh), which writes raw bytes to a fixed physical address (0x400000)   */
/* independently of the Multiboot protocol.                                  */
.section .multiboot, "a"
.align 4
mb1_start:
    .long  0x1BADB002                              /* magic              */
    .long  0x00010004                              /* flags: bit 2=video + bit 16=a.out */
    .long  -(0x1BADB002 + 0x00010004)             /* checksum           */
    /* a.out kludge address fields (required when bit 16 is set) */
    .long  mb1_start    /* header_addr: VA of this header (= load base)  */
    .long  mb1_start    /* load_addr:   load image to this physical addr  */
    .long  KERNEL_END   /* load_end_addr: stop here — skips ELF debug    */
                        /*  sections that follow KERNEL_END in the file, */
                        /*  keeping physical memory above KERNEL_END free */
                        /*  for the lythd module loaded via -device       */
    .long  0            /* bss_end_addr:  0 = boot.s handles BSS         */
    .long  _start       /* entry_addr:    jump here after load            */
    /* video mode fields (required when bit 2 is set) */
    .long  0            /* mode_type: 0 = linear graphics framebuffer     */
    .long  1024         /* preferred width  (bootloader may choose other) */
    .long  768          /* preferred height                               */
    .long  32           /* preferred depth (bits per pixel)               */
mb1_end:

/* ── Multiboot2 header (GRUB2) ─────────────────────────────────────────── */
/* Must be 8-byte aligned and within the first 32768 bytes of the image.    */
.align 8
mb2_start:
    .long  0xE85250D6                                    /* magic          */
    .long  0                                             /* arch: i386     */
    .long  (mb2_end - mb2_start)                         /* header length  */
    .long  -(0xE85250D6 + 0 + (mb2_end - mb2_start))    /* checksum       */
    /* required end tag */
    .short 0
    .short 0
    .long  8
mb2_end:

/* ── 32-bit protected-mode entry ───────────────────────────────────────── */
.section .boot, "ax"
.code32
.global _start
_start:
    cli

    /* Stash Multiboot arguments in callee-saved registers so the BSS clear
     * below doesn't clobber them (mb_magic/mb_info_ptr live in .bss).     */
    mov  %eax, %esi          /* ESI = mb_magic    */
    mov  %ebx, %ebp          /* EBP = mb_info_ptr */

    /* ── Zero the entire BSS section ─────────────────────────────────── */
    /* The Multiboot header sets bss_end_addr=0 (skip loader zeroing), so
     * we must zero BSS ourselves.  This covers page-table frames, the boot
     * stack, and ALL Rust statics (AtomicU64, static mut, etc.).          */
    mov  $__bss_start, %edi
    xor  %eax, %eax
    mov  $__bss_end,   %ecx
    sub  %edi, %ecx         /* byte count */
    add  $3,   %ecx         /* round up to next dword */
    shr  $2,   %ecx         /* dword count (ceiling division) */
    rep  stosl

    /* Now write the saved Multiboot values into their (now-zeroed) slots. */
    mov  %esi, mb_magic
    mov  %ebp, mb_info_ptr

    /* Set up a temporary stack in our BSS stack region (now zeroed) */
    mov  $boot_stack_top, %esp

    /* ── Build page tables (BSS already zeroed above) ────────────────── */

    /* P4[0] → P3  (present | writable) */
    mov  $p3_table, %eax
    or   $3, %eax
    mov  %eax, (p4_table)

    /* P3[0] → P2  (present | writable) */
    mov  $p2_table, %eax
    or   $3, %eax
    mov  %eax, (p3_table)

    /* P2[0..511]: 2 MiB huge-page entries covering the first 1 GiB
     *   entry = (n << 21) | 0x83  (present | writable | huge)
     *   Upper 32 bits of each 8-byte entry remain 0 (zeroed above).       */
    xor  %ecx, %ecx
.Lmap_huge_pages:
    mov  %ecx, %eax
    shl  $21, %eax          /* n × 2 MiB physical base */
    or   $0x83, %eax        /* present | writable | huge */
    mov  %eax, p2_table(, %ecx, 8)
    inc  %ecx
    cmp  $512, %ecx
    jl   .Lmap_huge_pages

    /* ── Enable PAE (CR4.PAE = bit 5) ────────────────────────────────── */
    mov  %cr4, %eax
    or   $0x20, %eax
    mov  %eax, %cr4

    /* ── Point CR3 at the P4 table ───────────────────────────────────── */
    mov  $p4_table, %eax
    mov  %eax, %cr3

    /* ── Set EFER.LME (bit 8) via MSR 0xC0000080 ─────────────────────── */
    mov  $0xC0000080, %ecx
    rdmsr
    or   $0x100, %eax
    wrmsr

    /* ── Enable paging (CR0.PG = bit 31); PE is already set by GRUB ──── */
    mov  %cr0, %eax
    or   $0x80000000, %eax
    mov  %eax, %cr0

    /* ── Load 64-bit GDT ─────────────────────────────────────────────── */
    lgdt gdt64_ptr

    /* ── Far jump to 64-bit long mode ────────────────────────────────── */
    /* CS ← 0x08 (64-bit code descriptor), EIP ← .Llong_mode_entry       */
    ljmp $0x08, $.Llong_mode_entry

/* ── 64-bit entry (same section, decoded as 64-bit after the far jump) ── */
.code64
.Llong_mode_entry:
    /* Reload data-segment registers with the data selector (0x10) */
    mov  $0x10, %ax
    mov  %ax, %ds
    mov  %ax, %es
    mov  %ax, %fs
    mov  %ax, %gs
    mov  %ax, %ss

    /* Zero rbp to mark the outermost stack frame */
    xor  %rbp, %rbp

    /* Pass Multiboot info to kmain(mb_magic: u32, mb_info: u64).
     * SysV AMD64 ABI: arg0 → RDI (u32 via EDI, zero-extends),
     *                 arg1 → RSI (u64 from 32-bit physical pointer). */
    movl mb_magic,    %edi
    movl mb_info_ptr, %esi
    call kmain

    /* kmain() must not return; spin-halt if it does */
.Lhalt:
    hlt
    jmp  .Lhalt

/* ── 64-bit GDT ────────────────────────────────────────────────────────── */
.section .rodata
.align 8
gdt64:
    .quad 0x0000000000000000   /* 0x00 — null descriptor                  */
    .quad 0x00AF9A000000FFFF   /* 0x08 — 64-bit code, ring 0 (L=1, D=0)   */
    .quad 0x00CF92000000FFFF   /* 0x10 — data, ring 0                     */
gdt64_end:

/* 6-byte GDT descriptor loaded from 32-bit mode (base is 32-bit) */
gdt64_ptr:
    .short (gdt64_end - gdt64 - 1)
    .long  gdt64

/* ── BSS: page tables + boot stack + saved Multiboot registers ─────────── */
.section .bss
.align 4096
p4_table:       .skip 4096
p3_table:       .skip 4096
p2_table:       .skip 4096

.align 16
boot_stack_bottom:
                .skip 131072    /* 128 KiB — debug builds use deep frames during disk I/O + ISR */
boot_stack_top:

/* Saved at 32-bit entry; read as 64-bit in long mode (upper half stays 0). */
.align 4
mb_magic:       .long 0
mb_info_ptr:    .long 0
