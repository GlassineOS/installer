#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-or-later
"""The placeholder boot loader the PC install cases put on the boot partition:
a minimal x86-64 UEFI application made here byte by byte (no tool, no vendor
file). It prints MARKER on the firmware's console (OVMF copies it to the
serial port) and powers the machine off. It stands in for GlassineOS's loader
(GlassineOS/boot, boot 010), which a release supplies as its `boot-loader`
role; the installer writes whichever the release holds."""

import struct
import sys

MARKER = "GLASSINE-PLACEHOLDER-LOADER started"


def efi_app(text: str = MARKER) -> bytes:
    msg = (text + "\r\n").encode("utf-16-le") + b"\0\0"
    code = bytearray()
    code += b"\x53"  # push rbx (the stack is 16-byte aligned again)
    code += b"\x48\x83\xec\x20"  # sub rsp, 32 (the callee's shadow space)
    code += b"\x48\x89\xd3"  # mov rbx, rdx (the system table)
    code += b"\x48\x8b\x4b\x40"  # mov rcx, [rbx+64] (ConOut)
    lea_at = len(code)
    code += b"\x48\x8d\x15\0\0\0\0"  # lea rdx, [rip+msg]
    code += b"\xff\x51\x08"  # call [rcx+8] (OutputString)
    code += b"\x48\x8b\x43\x58"  # mov rax, [rbx+88] (RuntimeServices)
    code += b"\xb9\x02\0\0\0"  # mov ecx, 2 (EfiResetShutdown)
    code += b"\x31\xd2"  # xor edx, edx (EFI_SUCCESS)
    code += b"\x45\x31\xc0"  # xor r8d, r8d (no data)
    code += b"\x45\x31\xc9"  # xor r9d, r9d
    code += b"\xff\x50\x68"  # call [rax+104] (ResetSystem)
    code += b"\xeb\xfe"  # jmp $ (never reached)
    rel = len(code) - (lea_at + 7)
    code[lea_at + 3 : lea_at + 7] = struct.pack("<i", rel)
    code += msg
    raw = bytes(code) + b"\0" * (-len(code) % 0x200)

    dos = bytearray(0x40)
    dos[0:2] = b"MZ"
    dos[0x3C:0x40] = struct.pack("<I", 0x40)
    coff = struct.pack("<HHIIIHH", 0x8664, 1, 0, 0, 0, 0xF0, 0x0022)
    opt = struct.pack(
        "<HBBIIIIIQIIHHHHHHIIIIHHQQQQII",
        0x20B, 0, 0,  # PE32+
        len(raw), 0, 0,  # code, data, bss
        0x1000, 0x1000,  # entry point, base of code
        0x10000000,  # image base (the code is position-independent; no relocations)
        0x1000, 0x200,  # section and file alignment
        0, 0, 0, 0, 0, 0,  # OS, image and subsystem versions
        0,  # Win32VersionValue
        0x1000 + ((len(code) + 0xFFF) & ~0xFFF),  # size of image
        0x200,  # size of headers
        0,  # checksum
        10,  # EFI application
        0,  # DLL characteristics
        0x10000, 0x10000, 0x10000, 0x10000,  # stack and heap
        0,  # loader flags
        16,  # data directories (all empty)
    ) + b"\0" * (16 * 8)
    sec = struct.pack(
        "<8sIIIIIIHHI", b".text", len(code), 0x1000, len(raw), 0x200, 0, 0, 0, 0, 0x60000020
    )
    head = bytes(dos) + b"PE\0\0" + coff + opt + sec
    assert len(head) <= 0x200
    return head + b"\0" * (0x200 - len(head)) + raw


if __name__ == "__main__":
    sys.stdout.buffer.write(efi_app())
