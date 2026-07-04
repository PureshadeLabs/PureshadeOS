/* boot.s — replaced by the Limine native boot protocol.
 *
 * The 32→64 stub, Multiboot1/2 headers, BSS-zeroing loop, and page-table
 * bootstrap that previously lived here are all unnecessary under Limine:
 *   - Limine enters the kernel in 64-bit long mode with interrupts off.
 *   - Limine zeros .bss before calling kernel_main.
 *   - Limine installs a valid PML4 (identity 0→4 GiB + HHDM).
 *   - Limine provides a ≥ 64 KiB stack on entry.
 *
 * The Limine request/response statics and the kernel_main entry point
 * now live in kernel/src/main.rs.
 *
 * This file is kept as a tombstone so the global_asm! inclusion in main.rs
 * still resolves without breaking the build.  It may be removed once the
 * global_asm! call is cleaned up.
 */
