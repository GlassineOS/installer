/* SPDX-License-Identifier: GPL-2.0-or-later */
/*
 * One PT_LOAD segment, filesz == memsz (no .bss to zero: whether the boot ROM zero-fills
 * memsz > filesz is unknown), linked at 0x14015000: on-chip RAM that a programmer the boot
 * ROM accepts on this SoC also loads to (layout fact from its ELF program headers only,
 * see RESEARCH.md). The image stays far below 64 KiB.
 */
ENTRY(_start)

MEMORY
{
    IMEM (rwx) : ORIGIN = 0x14015000, LENGTH = 0x10000
}

PHDRS
{
    image PT_LOAD FLAGS(7);
}

SECTIONS
{
    .text : {
        KEEP(*(.text.entry))
        *(.text .text.*)
    } >IMEM :image

    .rodata : ALIGN(16) {
        *(.rodata .rodata.*)
    } >IMEM :image

    .data : ALIGN(64) {
        /* The program's statics are placed in .data.* (PROGBITS), so the zeros of the stack
           below are written into the file too. A .bss input here would turn it NOBITS. */
        *(.data .data.*)
        *(.bss .bss.*)
        *(COMMON)
        . = ALIGN(16);
        __stack_bottom = .;
        . += 0x4000;
        __stack_top = .;
    } >IMEM :image

    /DISCARD/ : {
        *(.xblsec_tramp)
        *(.comment) *(.note .note.*) *(.eh_frame .eh_frame_hdr) *(.ARM.exidx*)
    }
}
