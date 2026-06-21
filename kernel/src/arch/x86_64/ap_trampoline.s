/*
 * ap_trampoline.s — Application Processor startup trampoline.
 *
 * The SIPI vector byte selects a 4 KiB page below 1 MiB as the AP entry
 * point.  We use physical 0x8000 (SIPI vector = 0x08).
 *
 * APs start in 16-bit real mode.  This trampoline:
 *   1. Loads a minimal GDT and enters 32-bit protected mode.
 *   2. Enables PAE, loads the BSP's PML4 into CR3, sets EFER.LME, enables
 *      paging → 64-bit long mode.
 *   3. Loads a per-AP kernel stack and calls ap_entry() (Rust).
 *
 * The code and embedded GDT must fit in 4 KiB.  The per-AP data area lives
 * at the end of the page (offsets 0xFD0–0xFFF).
 *
 * All absolute addresses in the 16-bit and 32-bit sections reference the
 * page as if it were at physical 0x8000 — i.e. they use 0x8xxx constants
 * directly.  The Rust smp::init() copies these bytes to 0x8000 at runtime
 * before sending SIPI.
 *
 * Page layout:
 *   0x8000–0x801F  16-bit real-mode startup code
 *   0x8020–0x8037  padding
 *   0x8038–0x803D  GDT pointer for lgdt in 16-bit mode  (limit + 32-bit base)
 *   0x803E–0x803F  padding
 *   0x8040–0x806F  GDT entries (6 × 8 bytes = 48 bytes)
 *                    0x8040: null
 *                    0x8048: 32-bit code, DPL=0
 *                    0x8050: 32-bit data, DPL=0
 *                    0x8058: 64-bit code, DPL=0 (L=1)
 *                    0x8060: 64-bit data, DPL=0
 *                    0x8068: unused slot (padding to keep ptr aligned)
 *   0x8070–0x8075  GDT pointer for lgdt in 32-bit mode  (limit + 32-bit base)
 *   0x8076–0x80FF  padding
 *   0x8100–0x817F  32-bit protected-mode code
 *   0x8200–0x827F  64-bit long-mode code
 *   0x8FD0         ap_entry fn pointer   (8 bytes, set by BSP before SIPI)
 *   0x8FD8         per-AP kernel stack top (8 bytes, set by BSP before SIPI)
 *   0x8FE0         BSP PML4 physical address (8 bytes, set by BSP before SIPI)
 *   0x8FE8         AP online flag (1 byte, written by AP to signal startup)
 */

.section .text.ap_trampoline, "ax"

.code16
.global ap_trampoline_start
ap_trampoline_start:
    cli
    cld
    xorw  %ax, %ax
    movw  %ax, %ds
    movw  %ax, %es

    /* Load the temporary GDT from offset 0x38 in this page.
     * `data32 lgdt` emits the 32-bit-base form (m16&32) in 16-bit mode. */
    data32 lgdt  0x8038

    /* Set CR0.PE to enter 32-bit protected mode. */
    movl  %cr0, %eax
    orl   $1, %eax
    movl  %eax, %cr0

    /* Far jump to flush the prefetch queue and reload CS with the 32-bit
     * code descriptor (selector 0x08).  Target EIP = 0x8100. */
    .byte  0xEA           /* far jmp opcode */
    .long  0x00008100     /* EIP (32-bit immediate; runs at 0x8100) */
    .short 0x0008         /* CS  = selector 0x08 (32-bit code) */

/* ── Pad + embed GDT data ─────────────────────────────────────────────── */

    /* The assembler places code above starting at section offset 0.  We need
     * the GDT pointer at offset 0x38 and the GDT at 0x40.  Pad with zeros. */
    .org 0x38
    /* GDT pointer (6 bytes): limit = 47 (6 entries × 8 − 1), base = 0x8040 */
    .short 47
    .long  0x00008040

    .org 0x40
    /* null descriptor */
    .quad 0
    /* 32-bit code: base=0, limit=4 GB, DPL=0, type=code, G=1, D=1 */
    .quad 0x00CF9A000000FFFF
    /* 32-bit data: base=0, limit=4 GB, DPL=0, type=data, G=1, D=1 */
    .quad 0x00CF92000000FFFF
    /* 64-bit code: base=0, limit=4 GB, DPL=0, type=code, G=1, L=1 */
    .quad 0x00AF9A000000FFFF
    /* 64-bit data (same as 32-bit data entry) */
    .quad 0x00CF92000000FFFF
    /* unused slot */
    .quad 0

    /* GDT pointer for use from 32-bit code (same GDT, same 32-bit base). */
    .org 0x70
    .short 47
    .long  0x00008040

/* ── 32-bit protected-mode code ─────────────────────────────────────────── */

    .org 0x100
.code32
    /* Reload data-segment registers with the 32-bit data selector (0x10). */
    movw  $0x10, %ax
    movw  %ax, %ds
    movw  %ax, %es
    movw  %ax, %ss

    /* Enable PAE (CR4[5]). */
    movl  %cr4, %eax
    orl   $0x20, %eax
    movl  %eax, %cr4

    /* Load the BSP's PML4 physical address into CR3.
     * The 64-bit value at 0x8FE0 fits in 32 bits on any machine with < 4 GiB
     * of physical RAM — which is all we support currently. */
    movl  0x8FE0, %eax
    movl  %eax, %cr3

    /* Set EFER.LME (bit 8) to activate long mode when paging is enabled. */
    movl  $0xC0000080, %ecx
    rdmsr
    orl   $0x100, %eax
    wrmsr

    /* Enable paging (CR0.PG = bit 31).  Long mode activates. */
    movl  %cr0, %eax
    orl   $0x80000000, %eax
    movl  %eax, %cr0

    /* Reload GDT with the same GDT but using the 32-bit-mode lgdt form. */
    lgdt  0x8070

    /* Far jump to 64-bit code: CS = 0x18 (64-bit code selector). */
    .byte  0xEA
    .long  0x00008200
    .short 0x0018

/* ── 64-bit long-mode code ─────────────────────────────────────────────── */

    .org 0x200
.code64
    /* Set up 64-bit data segments (use 0 for DS/ES/SS in long mode). */
    xorl  %eax, %eax
    movw  %ax, %ds
    movw  %ax, %es
    movw  %ax, %ss
    movw  %ax, %fs
    movw  %ax, %gs

    /* Load per-AP kernel stack from 0x8FD8. */
    movabsq $0x8FD8, %rax
    movq    (%rax), %rsp

    /* Load ap_idx from 0x8FE8 into %rdi (SysV arg 1 for ap_entry). */
    movabsq $0x8FE8, %rax
    movq    (%rax), %rdi

    /* Call ap_entry (address at 0x8FD0). */
    movabsq $0x8FD0, %rax
    movq    (%rax), %rax
    callq   *%rax

    /* ap_entry must not return; hlt-loop as safety net. */
.Lap_hlt:
    hlt
    jmp   .Lap_hlt

.global ap_trampoline_end
ap_trampoline_end:
