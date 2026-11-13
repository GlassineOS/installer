// SPDX-License-Identifier: GPL-2.0-or-later
//! Spike: the smallest programmer that proves the SDM636 boot ROM ran it.
//!
//! DRAFT, never run on hardware. What it does once the boot ROM jumps to `_start`:
//!
//! 1. Masks interrupts, takes its own stack and exception vectors, records the CPU state
//!    it was handed (exception level, SCTLR, VBAR, DAIF before masking, counter frequency).
//! 2. Reuses the USB device the boot ROM enumerated (05c6:9008, DWC3 core at 0x0a800000):
//!    finds the first enabled bulk-IN endpoint and sends one 128-byte status record on
//!    it. Nothing is re-initialised.
//! 3. After 5 s, soft-disconnects (DCTL.RUN_STOP = 0): the host sees 9008 vanish.
//! 4. After 15 s, resets through PS_HOLD; if still running, through a watchdog bite;
//!    then waits for events forever.
//!
//! Variant B adds an XBL_SEC-typed segment that only jumps to `_start` (see RESEARCH.md,
//! "the XBL_SEC question"); the status record says which entry was taken.
//!
//! Any exception goes to step 3 and 4 at once. It never touches storage: there is no
//! eMMC code at all.
//!
//! Register offsets come from public Linux sources (see RESEARCH.md for every path).

#![no_std]
#![no_main]

use core::arch::{asm, global_asm};
use core::panic::PanicInfo;
use core::ptr::{read_volatile, write_volatile};

// Addresses: arch/arm64/boot/dts/qcom/sdm630.dtsi (sdm636 includes it).
const DWC3: usize = 0x0a80_0000; // usb3_dwc3: usb@a800000, the device's port (qusb2phy0)
const PS_HOLD: usize = 0x010a_c000; // restart@10ac000, "qcom,pshold"
const APSS_WDT: usize = 0x1781_7000; // kpss-wdt (stock DT qcom,wdt@17817000)

// DWC3 registers: drivers/usb/dwc3/core.h.
const GSNPSID: usize = 0xc120;
const GEVNTADRLO0: usize = 0xc400;
const GEVNTADRHI0: usize = 0xc404;
const GEVNTSIZ0: usize = 0xc408;
const GEVNTCOUNT0: usize = 0xc40c;
const DCFG: usize = 0xc700;
const DCTL: usize = 0xc704;
const DSTS: usize = 0xc70c;
const DALEPENA: usize = 0xc720;
const fn depcmdpar2(n: usize) -> usize {
    0xc800 + n * 0x10
}
const fn depcmdpar1(n: usize) -> usize {
    0xc800 + n * 0x10 + 0x4
}
const fn depcmdpar0(n: usize) -> usize {
    0xc800 + n * 0x10 + 0x8
}
const fn depcmd(n: usize) -> usize {
    0xc800 + n * 0x10 + 0xc
}
const DCTL_RUN_STOP: u32 = 1 << 31;
const DSTS_DEVCTRLHLT: u32 = 1 << 22;
const DEPCMD_STARTTRANSFER: u32 = 0x06;
const DEPCMD_ENDTRANSFER: u32 = 0x08;
const DEPCMD_CMDACT: u32 = 1 << 10;
const DEPCMD_HIPRI_FORCERM: u32 = 1 << 11;
const fn depcmd_status(v: u32) -> u32 {
    (v >> 12) & 0xf
}
const fn depcmd_rsc_idx(v: u32) -> u32 {
    (v >> 16) & 0x7f
}
const TRB_HWO: u32 = 1 << 0;
const TRB_LST: u32 = 1 << 1;
const TRB_NORMAL: u32 = 1 << 4;
const TRB_IOC: u32 = 1 << 11;

// qcom-wdt.c, reg_offset_data_kpss. Clocked by the 32 768 Hz sleep clock.
const WDT_RST: usize = 0x4;
const WDT_EN: usize = 0x8;
const WDT_BARK_TIME: usize = 0x10;
const WDT_BITE_TIME: usize = 0x14;

// Timeline, from entry.
const DISCONNECT_AFTER_MS: u64 = 5_000;
const RESET_AFTER_MS: u64 = 15_000;
const SEND_TIMEOUT_MS: u64 = 4_000;

const STATUS_WORDS: usize = 32; // 128 bytes: a short packet at any max packet size

// flags in word 5 of the status record
const F_COUNTER_TICKS: u32 = 1 << 0;
const F_EP_FOUND: u32 = 1 << 1;
const F_START_TIMEOUT: u32 = 1 << 2;
const F_RETRIED: u32 = 1 << 3;
const F_VIA_XBL_SEC: u32 = 1 << 4;

const TRAMPOLINE_MAGIC: u64 = 0x474c_534e_5853_454b; // set by the trampoline in x19

#[repr(C, align(64))]
struct Dma {
    trb: [u32; 4],
    _pad: [u32; 12],
    buf: [u32; STATUS_WORDS],
}

// All state in a PROGBITS section, so the one LOAD segment has filesz == memsz: whether
// the boot ROM zero-fills memsz beyond filesz is unknown, so nothing relies on it.
#[unsafe(link_section = ".data.glassine")]
static mut DMA: Dma = Dma { trb: [0; 4], _pad: [0; 12], buf: [0; STATUS_WORDS] };
#[unsafe(link_section = ".data.glassine")]
static mut IN_EXCEPTION: u32 = 0;
#[unsafe(link_section = ".data.glassine")]
static mut COUNTER_TICKS: bool = false;
#[unsafe(link_section = ".data.glassine")]
static mut T0: u64 = 0;

global_asm!(
    r#"
    .section .text.entry, "ax"
    .global _start
_start:
    mrs     x20, daif           // as the boot ROM left it
    msr     daifset, #0xf
    ldr     x0, =__stack_top
    mov     sp, x0
    mrs     x0, CurrentEL
    ubfx    x0, x0, #2, #2
    adr     x2, glassine_vectors
    cmp     x0, #3
    b.ne    1f
    mrs     x1, vbar_el3
    msr     vbar_el3, x2
    b       4f
1:  cmp     x0, #2
    b.ne    2f
    mrs     x1, vbar_el2
    msr     vbar_el2, x2
    b       4f
2:  cmp     x0, #1
    b.ne    3f
    mrs     x1, vbar_el1
    msr     vbar_el1, x2
    b       4f
3:  mov     x1, #0
4:  isb
    mov     x2, x19             // = TRAMPOLINE_MAGIC when entered through the XBL_SEC trampoline
    mov     x3, x20
    bl      glassine_main
5:  wfe
    b       5b

    .section .text.vectors, "ax"
    .balign 0x800
glassine_vectors:
    .irp n, 0,1,2,3,4,5,6,7,8,9,10,11,12,13,14,15
    .balign 0x80
    mov     x0, #\n
    b       glassine_exception_entry
    .endr

glassine_exception_entry:
    ldr     x1, =__stack_top
    mov     sp, x1
    bl      glassine_exception
6:  wfe
    b       6b

"#
);

// The XBL_SEC-typed segment (link-xblsec.ld; variant B and the probes). The boot ROM starts
// execution here (device run 2026-11-13: B != E/D/F, which differ only in these bytes). Each
// variant is written out word for word, so a build reproduces the images that ran.
#[cfg(all(feature = "xbl-sec-trampoline", not(any(feature = "probe-spin",
    feature = "probe-pshold", feature = "probe-ret"))))]
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

#[inline(always)]
fn rd(addr: usize) -> u32 {
    unsafe { read_volatile(addr as *const u32) }
}

#[inline(always)]
fn wr(addr: usize, v: u32) {
    unsafe { write_volatile(addr as *mut u32, v) }
}

fn counter() -> u64 {
    let v: u64;
    unsafe { asm!("isb", "mrs {}, cntpct_el0", out(reg) v) };
    v
}

fn counter_freq() -> u64 {
    let v: u64;
    unsafe { asm!("mrs {}, cntfrq_el0", out(reg) v) };
    // Qualcomm's always-on counter runs at 19.2 MHz (XO); used when CNTFRQ is unset.
    if v == 0 { 19_200_000 } else { v }
}

/// Milliseconds since entry. Falls back to counting calls when the counter is stopped
/// (assumes ~1 µs per call: rough, only bounds the timeline).
fn ms_since_t0() -> u64 {
    unsafe {
        if COUNTER_TICKS {
            (counter() - T0) / (counter_freq() / 1000)
        } else {
            T0 += 1;
            T0 / 1000
        }
    }
}

fn wait_until_ms(t: u64) {
    while ms_since_t0() < t {
        core::hint::spin_loop();
    }
}

fn delay_ms(d: u64) {
    let end = ms_since_t0() + d;
    wait_until_ms(end);
}

fn dcache_line() -> usize {
    let ctr: u64;
    unsafe { asm!("mrs {}, ctr_el0", out(reg) ctr) };
    4 << ((ctr >> 16) & 0xf)
}

/// Clean + invalidate to the point of coherency. Harmless with caches off.
fn dc_civac(start: usize, len: usize) {
    let line = dcache_line();
    let mut a = start & !(line - 1);
    while a < start + len {
        unsafe { asm!("dc civac, {}", in(reg) a) };
        a += line;
    }
    unsafe { asm!("dsb sy") };
}

fn el_sctlr(el: u64) -> u64 {
    let sctlr: u64;
    unsafe {
        match el {
            3 => asm!("mrs {}, sctlr_el3", out(reg) sctlr),
            2 => asm!("mrs {}, sctlr_el2", out(reg) sctlr),
            1 => asm!("mrs {}, sctlr_el1", out(reg) sctlr),
            _ => sctlr = 0,
        }
    }
    sctlr
}

fn put(i: usize, v: u32) {
    unsafe { write_volatile((&raw mut DMA.buf).cast::<u32>().add(i), v) }
}

fn put64(i: usize, v: u64) {
    put(i, v as u32);
    put(i + 1, (v >> 32) as u32);
}

/// Issue an endpoint command and wait for CMDACT to clear. Returns the final DEPCMD, or
/// None on timeout.
fn ep_command(ep: usize, cmd: u32, p0: u32, p1: u32) -> Option<u32> {
    wr(depcmdpar2(ep), 0);
    wr(depcmdpar1(ep), p1);
    wr(depcmdpar0(ep), p0);
    wr(depcmd(ep), cmd | DEPCMD_CMDACT);
    let end = ms_since_t0() + 50;
    loop {
        let v = rd(depcmd(ep));
        if v & DEPCMD_CMDACT == 0 {
            return Some(v);
        }
        if ms_since_t0() > end {
            return None;
        }
    }
}

/// Consume whatever the controller logged into the boot ROM's event buffer, so it never
/// fills. The events are not parsed: completion is read from the TRB.
fn drain_events() {
    let n = rd(DWC3 + GEVNTCOUNT0) & 0xfffc;
    if n != 0 {
        wr(DWC3 + GEVNTCOUNT0, n);
    }
}

/// Queue the status record on bulk-IN endpoint `ep` and wait for the host to take it.
fn send_status(ep: usize, flags: &mut u32) -> bool {
    let dma = &raw mut DMA;
    let trb = unsafe { (&raw mut (*dma).trb).cast::<u32>() };
    let buf = unsafe { (&raw mut (*dma).buf) as usize };
    let trb_addr = trb as usize;
    let len = (STATUS_WORDS * 4) as u32;

    let before = rd(DWC3 + depcmd(ep));
    put(24, before);

    for attempt in 0..2u32 {
        put(5, *flags);
        put(25, attempt);
        unsafe {
            write_volatile(trb.add(0), buf as u32); // BPL
            write_volatile(trb.add(1), (buf as u64 >> 32) as u32); // BPH (0: IMEM is < 4 GiB)
            write_volatile(trb.add(2), len); // size, PCM1 = 0
            write_volatile(trb.add(3), TRB_HWO | TRB_LST | TRB_NORMAL | TRB_IOC);
        }
        dc_civac(dma as usize, core::mem::size_of::<Dma>());

        let r = ep_command(
            ep,
            DEPCMD_STARTTRANSFER,
            (trb_addr as u64 >> 32) as u32,
            trb_addr as u32,
        );
        let started = match r {
            Some(v) => depcmd_status(v) == 0,
            None => {
                *flags |= F_START_TIMEOUT;
                false
            }
        };
        if !started {
            // The boot ROM may still own a transfer on this endpoint: end it, start ours.
            put(26, r.unwrap_or(0xffff_ffff));
            *flags |= F_RETRIED;
            let _ = ep_command(
                ep,
                DEPCMD_ENDTRANSFER | DEPCMD_HIPRI_FORCERM | (depcmd_rsc_idx(before) << 16),
                0,
                0,
            );
            continue;
        }

        let end = ms_since_t0() + SEND_TIMEOUT_MS;
        while ms_since_t0() < end {
            drain_events();
            dc_civac(trb_addr, 16);
            if unsafe { read_volatile(trb.add(3)) } & TRB_HWO == 0 {
                return true;
            }
        }
        return false;
    }
    false
}

fn disconnect() {
    wr(DWC3 + DCTL, rd(DWC3 + DCTL) & !DCTL_RUN_STOP);
    let end = ms_since_t0() + 100;
    while rd(DWC3 + DSTS) & DSTS_DEVCTRLHLT == 0 && ms_since_t0() < end {
        drain_events();
    }
}

fn reset() -> ! {
    // PS_HOLD low: the PMIC resets or powers off, whichever its PS_HOLD config says
    // (as left by the boot ROM; unknown). drivers/power/reset/msm-poweroff.c.
    wr(PS_HOLD, 0);
    delay_ms(2_000);
    // Still here: watchdog bite in ~0.1 s (may land in the 900E crash-dump mode).
    wr(APSS_WDT + WDT_EN, 0);
    wr(APSS_WDT + WDT_BARK_TIME, 0x7fff_ffff);
    wr(APSS_WDT + WDT_BITE_TIME, 3_277);
    wr(APSS_WDT + WDT_RST, 1);
    wr(APSS_WDT + WDT_EN, 1);
    delay_ms(2_000);
    loop {
        unsafe { asm!("wfe") };
    }
}

#[unsafe(no_mangle)]
extern "C" fn glassine_main(el: u64, vbar_old: u64, via: u64, daif: u64) -> ! {
    let t = counter();
    let mut spin = 0u32;
    while spin < 100_000 {
        spin += 1;
        core::hint::spin_loop();
    }
    unsafe {
        COUNTER_TICKS = counter() != t;
        T0 = if COUNTER_TICKS { t } else { 0 };
    }
    let mut flags = if unsafe { COUNTER_TICKS } { F_COUNTER_TICKS } else { 0 };
    if via == TRAMPOLINE_MAGIC {
        flags |= F_VIA_XBL_SEC;
    }

    let sctlr = el_sctlr(el);
    let (midr, mpidr): (u64, u64);
    unsafe {
        asm!("mrs {}, midr_el1", out(reg) midr);
        asm!("mrs {}, mpidr_el1", out(reg) mpidr);
    }

    put(0, u32::from_le_bytes(*b"GLSN"));
    put(1, u32::from_le_bytes(*b"PRG0"));
    put(2, 1); // record version
    put(3, (STATUS_WORDS * 4) as u32);
    put(4, el as u32);
    put64(6, midr);
    put64(8, mpidr);
    put64(10, sctlr);
    put64(12, vbar_old);
    put64(14, daif);
    put64(16, counter_freq());
    put(18, rd(DWC3 + GSNPSID));
    put(19, rd(DWC3 + DCFG));
    put(20, rd(DWC3 + DCTL));
    put(21, rd(DWC3 + DSTS));
    let enabled = rd(DWC3 + DALEPENA);
    put(22, enabled);
    put(27, rd(DWC3 + GEVNTADRLO0));
    put(28, rd(DWC3 + GEVNTADRHI0));
    put(29, rd(DWC3 + GEVNTSIZ0));
    put(30, rd(DWC3 + GEVNTCOUNT0));
    put(31, glassine_main as *const () as usize as u32);

    // Physical endpoint n: n = 2 * number + direction; IN endpoints are odd, ep0 is 0/1.
    let ep = (3..32).step_by(2).find(|n| enabled & (1 << n) != 0);
    if let Some(ep) = ep {
        flags |= F_EP_FOUND;
        put(23, ep as u32);
        let _ = send_status(ep, &mut flags);
    } else {
        put(5, flags);
    }

    wait_until_ms(DISCONNECT_AFTER_MS);
    disconnect();
    wait_until_ms(RESET_AFTER_MS);
    reset()
}

#[unsafe(no_mangle)]
extern "C" fn glassine_exception(_vector: u64) -> ! {
    unsafe {
        if IN_EXCEPTION != 0 {
            // A fault inside the fault path: stop, the power key gets out.
            loop {
                asm!("wfe");
            }
        }
        IN_EXCEPTION = 1;
    }
    disconnect();
    reset()
}

#[panic_handler]
fn panic(_: &PanicInfo) -> ! {
    disconnect();
    reset()
}
