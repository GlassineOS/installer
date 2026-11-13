/* SPDX-License-Identifier: GPL-2.0-or-later */
/*
 * One PT_LOAD segment, filesz == memsz (no .bss to zero: whether the boot ROM zero-fills
 * memsz > filesz is unknown), linked at 0x14015000: on-chip RAM that a programmer the boot
 * ROM accepts on this SoC also loads to (layout fact from its ELF program headers only,
 * see RESEARCH.md). The image stays far below 64 KiB.
 */
/*
 * Variant B: the same image plus a second segment typed XBL_SEC (p_flags 0x05000005, the
 * type coreboot's createxbl.py gives Qualcomm's QC-Sec part) at 0x14699000, where the
 * accepted programmer has its XBL_SEC segment. It holds only a jump to _start.
 */
ENTRY(_start)

MEMORY
{
    IMEM (rwx) : ORIGIN = 0x14015000, LENGTH = 0x10000
    XBLSEC (rx) : ORIGIN = 0x14699000, LENGTH = 0x1000
}

PHDRS
{
    image PT_LOAD FLAGS(7);
    xblsec PT_LOAD FLAGS(0x05000005);
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

    .xblsec : {
        KEEP(*(.xblsec_tramp))
    } >XBLSEC :xblsec

    /DISCARD/ : {
        *(.comment) *(.note .note.*) *(.eh_frame .eh_frame_hdr) *(.ARM.exidx*)
    }
}
