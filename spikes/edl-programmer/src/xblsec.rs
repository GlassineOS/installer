// SPDX-License-Identifier: GPL-2.0-or-later
//! The XBL_SEC-typed segment (link-xblsec.ld; every variant but A).
//!
//! The boot ROM starts execution here (device run 2026-11-13: B differs from E/D/F, which
//! differ from it only in these bytes). Each variant is written out word for word, so a
//! build reproduces the images that ran. The probes (all but the trampoline) never reach
//! `_start`: they are self-contained and only measure, through the time at which the chip
//! resets, one thing each. TESTPLAN.md has the expected times.
//!
//! Timebase of the probes: CNTPCT_EL0 counted at 19.2 MHz (sdm630.dtsi: the arch timer
//! runs from `xo_board`, 19.2 MHz). Probe U verifies that rate.

use core::arch::global_asm;

// B: the trampoline. Everything that is not a probe uses it.
#[cfg(not(any(
    feature = "probe-spin",
    feature = "probe-pshold",
    feature = "probe-ret",
    feature = "probe-unmask",
    feature = "probe-resets",
    feature = "probe-mask"
)))]
global_asm!(
    r#"
    .section .xblsec_tramp, "ax"
    .global glassine_xblsec_entry
glassine_xblsec_entry:
    ldr     x19, 1f             // TRAMPOLINE_MAGIC: _start knows it came this way
    ldr     x16, 2f
    br      x16
    .word   0
1:  .quad   0x474c534e5853454b
2:  .quad   _start
"#
);

// Probe E: spin at the XBL_SEC entry (device: bulk IN EPROTO at T0+4 ms, fresh 9008).
#[cfg(feature = "probe-spin")]
global_asm!(
    r#"
    .section .xblsec_tramp, "ax"
    .global glassine_xblsec_entry
glassine_xblsec_entry:
    b       .
    ldr     x16, 2f             // the rest is B's, unreachable
    br      x16
    .word   0
    .quad   0x474c534e5853454b
2:  .quad   _start
"#
);

// Probe F: return to whatever x30 holds (device: same as E).
#[cfg(feature = "probe-ret")]
global_asm!(
    r#"
    .section .xblsec_tramp, "ax"
    .global glassine_xblsec_entry
glassine_xblsec_entry:
    ret
    ldr     x16, 2f             // the rest is B's, unreachable
    br      x16
    .word   0
    .quad   0x474c534e5853454b
2:  .quad   _start
"#
);

// Probe D: PS_HOLD low, then spin (device: same as E). The tail is what was left of B's
// literals when the image was first patched by hand: kept so the build matches it.
#[cfg(feature = "probe-pshold")]
global_asm!(
    r#"
    .section .xblsec_tramp, "ax"
    .global glassine_xblsec_entry
glassine_xblsec_entry:
    movz    x0, #0xc000
    movk    x0, #0x010a, lsl #16
    str     wzr, [x0]
    dsb     sy
    b       .
    .word   0x474c534e
    .quad   _start
"#
);

// Probe G: mask everything and spin. No MMIO, no counter. Answers whether the 7.56 s reset
// of B comes from outside our code: reset at ~7.56 s = an external timer; none within the
// client's 20 s = it was our code (power hold to leave).
#[cfg(feature = "probe-mask")]
global_asm!(
    r#"
    .section .xblsec_tramp, "ax"
    .global glassine_xblsec_entry
glassine_xblsec_entry:
    msr     daifset, #0xf
1:  b       1b
"#
);

// Shared by the new probes: wait until CNTPCT_EL0 - x9 >= TICKS + x14.
#[cfg(any(feature = "probe-unmask", feature = "probe-resets"))]
global_asm!(
    r#"
    .macro  wait_ticks ticks
    ldr     x10, =\ticks
    add     x10, x10, x14
.Lw\@:
    isb
    mrs     x11, cntpct_el0
    sub     x11, x11, x9
    cmp     x11, x10
    b.lo    .Lw\@
    .endm
"#
);

// Probe U: is it the boot ROM's interrupts that reset E? Mask everything, wait 1.0 s
// (19.2 M ticks), unmask, spin. No MMIO, VBAR left as the boot ROM set it.
//   reset at ~4 ms : masking is not what keeps B alive
//   reset at ~1.0 s: masking is; unmasking resets (the "E path"); the counter is 19.2 MHz
//   other time t   : as above, counter at 19.2 MHz / t
//   7.56 s / none  : masking is; unmasking does nothing (or the counter does not run);
//                    a reset at 7.56 s is then not our code
#[cfg(feature = "probe-unmask")]
global_asm!(
    r#"
    .section .xblsec_tramp, "ax"
    .global glassine_xblsec_entry
glassine_xblsec_entry:
    msr     daifset, #0xf
    mov     x14, #0
    mrs     x9, cntpct_el0
    wait_ticks 19200000
    msr     daifclr, #0xf
    isb
1:  b       1b
    .ltorg
"#
);

// Probe R: which of PS_HOLD and the APSS watchdog resets? (after U: needs the E path and
// the 19.2 MHz counter). Masked throughout until the last step. The watchdog is first
// touched at 0.8 s, so a fault there cannot hide the PS_HOLD result. x14 = 0.2 s if the
// watchdog was already enabled at 0.8 s (by the boot ROM), else 0: it delays the bite.
//   0.5 s        : PS_HOLD low resets
//   0.8 s        : the watchdog register access faults (into the boot ROM's vectors)
//   1.2 s / 1.4 s: PS_HOLD does nothing; the watchdog bite (sleep clock) resets; it was
//                  off / already on before we touched it
//   2.0 s        : neither does; unmasking resets (the E path)
#[cfg(feature = "probe-resets")]
global_asm!(
    r#"
    .section .xblsec_tramp, "ax"
    .global glassine_xblsec_entry
glassine_xblsec_entry:
    msr     daifset, #0xf
    mrs     x9, cntpct_el0
    mov     x14, #0
    wait_ticks 9600000                  // 0.5 s
    movz    x0, #0xc000
    movk    x0, #0x010a, lsl #16        // PS_HOLD 0x010ac000
    str     wzr, [x0]
    dsb     sy
    wait_ticks 15360000                 // 0.8 s
    movz    x13, #0x7000
    movk    x13, #0x1781, lsl #16       // APSS watchdog 0x17817000
    ldr     w12, [x13, #0x8]            // WDT_EN as the boot ROM left it
    and     w12, w12, #1
    ldr     w15, =6554                  // 0.2 s at 32768 Hz
    mul     w15, w15, w12
    ldr     w0, =13107                  // 0.4 s at 32768 Hz
    add     w15, w15, w0
    str     wzr, [x13, #0x8]            // WDT_EN = 0
    mov     w0, #1
    str     w0, [x13, #0x4]             // WDT_RST
    ldr     w0, =0x0fffffff
    str     w0, [x13, #0x10]            // WDT_BARK_TIME: never (FIQ is masked anyway)
    str     w15, [x13, #0x14]           // WDT_BITE_TIME: 0.4 s (+0.2 s if it was on)
    mov     w0, #1
    str     w0, [x13, #0x8]             // WDT_EN
    dsb     sy
    wait_ticks 38400000                 // 2.0 s
    msr     daifclr, #0xf
    isb
1:  b       1b
    .ltorg
"#
);
