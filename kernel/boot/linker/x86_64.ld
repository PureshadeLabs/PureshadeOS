ENTRY(_start)

SECTIONS {
    . = 0x100000;

    KERNEL_START = .;

    /* Boot section first: Multiboot headers + 32→64 stub */
    .boot : {
        KEEP(*(.multiboot))
        *(.boot)
    }

    .text : ALIGN(4096) {
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
        *(COMMON)
        *(.bss .bss.*)
        __bss_end = .;
    }

    . = ALIGN(4096);
    KERNEL_END = .;

    /DISCARD/ : {
        *(.eh_frame)
        *(.note .note.*)
        *(.comment)
    }
}
