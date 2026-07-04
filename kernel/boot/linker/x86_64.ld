ENTRY(kernel_main)

PHDRS {
    requests PT_LOAD FLAGS(6) ; /* PF_R | PF_W — Limine writes responses here */
    text     PT_LOAD FLAGS(5) ; /* PF_R | PF_X */
    rodata   PT_LOAD FLAGS(4) ; /* PF_R */
    data     PT_LOAD FLAGS(6) ; /* PF_R | PF_W */
}

SECTIONS {
    /* Higher-half virtual base required by Limine v5+ protocol.
     * Physical load address is determined by Limine; the kernel receives
     * it via KernelAddressResponse and must not assume 0x100000. */
    . = 0xFFFFFFFF80100000;

    KERNEL_START = .;

    /* Limine scans these sections for request/response structs.
     * Limine writes response pointers back before handing control, so
     * this segment must be RW.  GOT lives here too (lld places it first). */
    .requests_start : ALIGN(4096) {
        KEEP(*(.requests_start))
    } :requests

    .got : {
        *(.got)
        *(.got.plt)
    } :requests

    .requests : {
        KEEP(*(.requests))
    } :requests

    .requests_end : {
        KEEP(*(.requests_end))
    } :requests

    .text : ALIGN(4096) {
        *(.text .text.*)
    } :text

    .rodata : ALIGN(4096) {
        *(.rodata .rodata.*)
    } :rodata

    .data : ALIGN(4096) {
        *(.data .data.*)
    } :data

    .bss : ALIGN(4096) {
        __bss_start = .;
        *(COMMON)
        *(.bss .bss.*)
        __bss_end = .;
    } :data

    . = ALIGN(4096);
    KERNEL_END = .;

    /DISCARD/ : {
        *(.eh_frame)
        *(.eh_frame_hdr)
        *(.note .note.*)
        *(.comment)
    }
}
