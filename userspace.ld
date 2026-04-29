/* Linker script for OROS userspace ELF64 programs.
 *
 * Load address: 0x0000_0001_0000_0000
 *   - Sits above the lythos kernel's 0→1 GiB identity-mapped region.
 *   - Matches the user VA convention used by lythos's ELF loader.
 *   - Each process runs in its own address space, so all programs may share
 *     this VA without conflict.
 *
 * BSS is zero-filled by the lythos ELF loader (p_memsz vs p_filesz), so no
 * runtime zero-init is required.
 */

ENTRY(_start)

SECTIONS {
    . = 0x0000000100000000;

    .text : {
        *(.text .text.*)
    }

    .rodata : ALIGN(4096) {
        *(.rodata .rodata.*)
    }

    .data : ALIGN(4096) {
        *(.data .data.*)
    }

    .bss : ALIGN(4096) {
        __bss_start = .;
        *(.bss .bss.*)
        *(COMMON)
        __bss_end = .;
        . = ALIGN(4096);
    }

    /DISCARD/ : {
        *(.eh_frame)
        *(.note .note.*)
        *(.comment)
    }
}
