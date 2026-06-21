/*
 * isr_stubs.s — GDT flush, ISR stubs for vectors 0–31, common dispatcher
 * AT&T syntax (included with options(att_syntax)).
 *
 * NOTE: boot.s ends with .section .bss; this file is processed in the same
 * LLVM translation unit, so we must reset to .text explicitly.
 */
.section .text

/* ── gdt_flush(ptr: *const GdtPtr) ──────────────────────────────────────── */
/* Loads a new GDT and reloads all segment registers, including CS.          */
/* Called from Rust: rdi = pointer to the 10-byte GdtPtr struct.             */
.global gdt_flush
gdt_flush:
    lgdt (%rdi)

    /* Reload CS via far return.  Push CS selector first (higher on stack),  */
    /* then the return address (lower); lretq pops RIP then CS.               */
    push $0x08
    lea  .Lgdt_cs_done(%rip), %rax
    push %rax
    lretq
.Lgdt_cs_done:
    mov $0x10, %ax
    mov %ax, %ds
    mov %ax, %es
    mov %ax, %fs
    mov %ax, %gs
    mov %ax, %ss
    ret

/* ── ISR stub macros ─────────────────────────────────────────────────────── */

/* Vector where the CPU does NOT push an error code: push dummy 0 first.     */
.macro isr_no_err num
.global isr_stub_\num
isr_stub_\num:
    push $0          /* dummy error code */
    push $\num       /* vector number    */
    jmp  exception_common
.endm

/* Vector where the CPU DOES push an error code.                             */
.macro isr_err num
.global isr_stub_\num
isr_stub_\num:
    push $\num       /* vector number (error code already on stack) */
    jmp  exception_common
.endm

/* Vectors 0–31 — Intel SDM Vol.3A Table 6-1                                */
isr_no_err 0    /* #DE  divide error                                        */
isr_no_err 1    /* #DB  debug                                               */
isr_no_err 2    /*      NMI                                                 */
isr_no_err 3    /* #BP  breakpoint                                          */
isr_no_err 4    /* #OF  overflow                                            */
isr_no_err 5    /* #BR  bound range exceeded                                */
isr_no_err 6    /* #UD  invalid opcode                                      */
isr_no_err 7    /* #NM  device not available                                */
isr_err     8   /* #DF  double fault            (error code always 0)      */
isr_no_err 9    /* coprocessor overrun (reserved)                           */
isr_err     10  /* #TS  invalid TSS                                         */
isr_err     11  /* #NP  segment not present                                 */
isr_err     12  /* #SS  stack-segment fault                                 */
isr_err     13  /* #GP  general protection fault                            */
isr_err     14  /* #PF  page fault                                          */
isr_no_err 15   /* reserved                                                 */
isr_no_err 16   /* #MF  x87 FPU floating-point error                       */
isr_err     17  /* #AC  alignment check                                     */
isr_no_err 18   /* #MC  machine check                                       */
isr_no_err 19   /* #XM  SIMD floating-point exception                      */
isr_no_err 20   /* #VE  virtualisation exception                            */
isr_err     21  /* #CP  control protection exception                        */
isr_no_err 22
isr_no_err 23
isr_no_err 24
isr_no_err 25
isr_no_err 26
isr_no_err 27
isr_no_err 28
isr_no_err 29
isr_err     30  /* #SX  security exception (AMD)                            */
isr_no_err 31

/* ── Common exception dispatcher ─────────────────────────────────────────── */
/* Stack on entry: [vector | error_code | rip | cs | rflags | ...]           */
exception_common:
    push %rax
    push %rbx
    push %rcx
    push %rdx
    push %rsi
    push %rdi
    push %rbp
    push %r8
    push %r9
    push %r10
    push %r11
    push %r12
    push %r13
    push %r14
    push %r15

    mov  %rsp, %rdi         /* arg0 = *const ExceptionFrame */
    call exception_handler  /* diverges (halts); iretq path kept for future use */

    pop  %r15
    pop  %r14
    pop  %r13
    pop  %r12
    pop  %r11
    pop  %r10
    pop  %r9
    pop  %r8
    pop  %rbp
    pop  %rdi
    pop  %rsi
    pop  %rdx
    pop  %rcx
    pop  %rbx
    pop  %rax

    add  $16, %rsp          /* discard vector + error_code */
    iretq

/* ── ISR address table (read by idt::init to fill the IDT) ──────────────── */
.section .rodata
.global isr_stub_table
isr_stub_table:
    .quad isr_stub_0,  isr_stub_1,  isr_stub_2,  isr_stub_3
    .quad isr_stub_4,  isr_stub_5,  isr_stub_6,  isr_stub_7
    .quad isr_stub_8,  isr_stub_9,  isr_stub_10, isr_stub_11
    .quad isr_stub_12, isr_stub_13, isr_stub_14, isr_stub_15
    .quad isr_stub_16, isr_stub_17, isr_stub_18, isr_stub_19
    .quad isr_stub_20, isr_stub_21, isr_stub_22, isr_stub_23
    .quad isr_stub_24, isr_stub_25, isr_stub_26, isr_stub_27
    .quad isr_stub_28, isr_stub_29, isr_stub_30, isr_stub_31
