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
        /* Pad this single load segment to a FIXED 0x5680 bytes. The SDM636 boot ROM is
           sensitive to this segment's size: it completes the Sahara handshake (END_IMAGE_TX,
           DONE_RESP) for a 0x5680-byte segment (every image that ran on 2026-11-13 morning)
           but HANGS after reading it when it is 0x5a00 (every image built once main.rs grew;
           device runs 2026-11-13 evening, RESEARCH.md). 0x5680 is the one proven-good size,
           and it puts the XBL_SEC segment at file offset 0x8000. The stack fills whatever is
           left below this fixed end, so code growth is absorbed by the stack, not the layout.
           Keep code+rodata+data well under ~0x3000 so the stack stays large. */
        . = ORIGIN(IMEM) + 0x5680;
        __stack_top = .;
    } >IMEM :image
    ASSERT(__stack_bottom <= ORIGIN(IMEM) + 0x5680 - 0x1000,
           "code+rodata+data overran the fixed 0x5680 load segment (need >=4 KiB stack)")

    .xblsec : {
        KEEP(*(.xblsec_tramp))
    } >XBLSEC :xblsec

    /DISCARD/ : {
        *(.comment) *(.note .note.*) *(.eh_frame .eh_frame_hdr) *(.ARM.exidx*)
    }
}
