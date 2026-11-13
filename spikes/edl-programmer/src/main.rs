// SPDX-License-Identifier: GPL-2.0-or-later
//! Spike: the smallest programmer that proves the SDM636 boot ROM ran it.
//!
//! What it does once the boot ROM has jumped (through the XBL_SEC trampoline, `xblsec.rs`)
//! to `_start`:
//!
//! 1. Masks interrupts, takes its own stack and exception vectors, records the CPU state
//!    it was handed (exception level, SCTLR, VBAR, DAIF before masking, the boot ROM's SP
//!    and LR, the counter).
//! 2. Reuses the USB device the boot ROM enumerated (05c6:9008, DWC3 core at 0x0a800000):
//!    finds the first enabled bulk-IN endpoint and sends one 192-byte status record on
//!    it. Nothing is re-initialised.
//! 3. Ends by resetting the chip at a time that says how far it got (a stage code), so
//!    the host learns that even when USB stays silent: the bulk-IN read fails at
//!    T(code) = 0.6 s + 0.5 s x code after DONE_RESP. See [`Code`] and TESTPLAN.md.
//!
//! With the `no-wdt` feature (image `programmer-n`) the watchdog is never touched.
//!
//! The reset at T(code) goes through a chain, because which mechanism works is still being
//! measured: PS_HOLD low at T; the APSS watchdog, armed at every stage marker, bites at
//! T + 0.15 s (it also catches a hang right after a marker); at T + 0.3 s the boot ROM's
//! vector table and interrupt mask are restored, which resets the chip the way probe E
//! did (the "E path"). The host reads which one fired from the offset.
//!
//! It never touches storage: there is no eMMC code at all.
//!
//! Register offsets come from public Linux sources (see RESEARCH.md for every path).

#![no_std]
#![no_main]

mod xblsec;

use core::arch::{asm, global_asm};
use core::panic::PanicInfo;
use core::ptr::{read_volatile, write_volatile};

// Addresses: arch/arm64/boot/dts/qcom/sdm630.dtsi (sdm636 includes it).
const DWC3: usize = 0x0a80_0000; // usb3_dwc3: usb@a800000, the device's port (qusb2phy0)
const PS_HOLD: usize = 0x010a_c000; // restart@10ac000, "qcom,pshold"
const APSS_WDT: usize = 0x1781_7000; // kpss-wdt (NA1 board DT; stock qcom,wdt@17817000)

// DWC3 registers: drivers/usb/dwc3/core.h (offsets from the core's base).
const GCTL: usize = 0xc110;
const GSNPSID: usize = 0xc120;
const GUSB2PHYCFG0: usize = 0xc200;
const GEVNTADRLO0: usize = 0xc400;
const GEVNTADRHI0: usize = 0xc404;
const GEVNTSIZ0: usize = 0xc408;
const GEVNTCOUNT0: usize = 0xc40c;
const DCFG: usize = 0xc700;
const DCTL: usize = 0xc704;
const DEVTEN: usize = 0xc708;
const DSTS: usize = 0xc70c;
const DALEPENA: usize = 0xc720;
const fn depcmdpar2(n: usize) -> usize {
    DWC3 + 0xc800 + n * 0x10
}
const fn depcmdpar1(n: usize) -> usize {
    DWC3 + 0xc800 + n * 0x10 + 0x4
}
const fn depcmdpar0(n: usize) -> usize {
    DWC3 + 0xc800 + n * 0x10 + 0x8
}
const fn depcmd(n: usize) -> usize {
    DWC3 + 0xc800 + n * 0x10 + 0xc
}
const GUSB2PHYCFG_SUSPHY: u32 = 1 << 6;
const GUSB2PHYCFG_ENBLSLPM: u32 = 1 << 8;
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
const WDT_HZ: u64 = 32_768;

/// The arch counter's rate: 19.2 MHz (sdm630.dtsi, `xo_board`). Fixed rather than read
/// from CNTFRQ_EL0, which the boot ROM may leave unset or wrong (its value is recorded);
/// probe U measures the real rate.
const TICKS_PER_MS: u64 = 19_200;

const SEND_TIMEOUT_MS: u64 = 1_000;

const STATUS_WORDS: usize = 48; // 192 bytes: a short packet at any max packet size

// flags in word 5 of the status record
const F_COUNTER_TICKS: u32 = 1 << 0;
const F_EP_FOUND: u32 = 1 << 1;
const F_START_TIMEOUT: u32 = 1 << 2;
const F_RETRIED: u32 = 1 << 3;
const F_VIA_XBL_SEC: u32 = 1 << 4;
const F_PHY_SUSPEND_CLEARED: u32 = 1 << 5;

const TRAMPOLINE_MAGIC: u64 = 0x474c_534e_5853_454b; // set by the trampoline in x19

/// Stage codes. The chip resets at T(code) = 0.6 s + 0.5 s x code after entry. Marker
/// codes are passed on the way; the watchdog is armed for each, so a hang right after one
/// shows as T + 0.15 s. End codes are where the program stops.
#[derive(Clone, Copy)]
#[repr(u32)]
enum Code {
    /// marker: `glassine_main` reached (stack, vectors and the masked DAIF in place)
    Entered = 0,
    /// marker: CPU state recorded, counter checked
    CpuState = 1,
    /// marker: the first DWC3 register read (GSNPSID) returned
    Dwc3Read = 2,
    /// end: GSNPSID is not a DWC3 / DWC3.x ID
    NotDwc3 = 3,
    /// end: the controller is not running (DCTL.RUN_STOP clear or DSTS.DEVCTRLHLT set)
    NotRunning = 4,
    /// end: no bulk-IN endpoint enabled in DALEPENA
    NoEndpoint = 5,
    /// marker: STARTTRANSFER about to be issued (a hang here = a register access stalls)
    Starting = 6,
    /// end: STARTTRANSFER refused or timed out, also after ENDTRANSFER + retry
    StartFailed = 7,
    /// end (and marker while waiting): transfer started, the host did not take it in 1 s
    NotTaken = 8,
    /// end: the TRB came back (HWO clear): the host has the record
    Sent = 9,
    /// end: exception or panic before any DWC3 access
    FaultEarly = 10,
    /// end: exception or panic during the DWC3 setup (codes 2..5)
    FaultSetup = 11,
    /// end: exception or panic during the transfer (codes 6..)
    FaultTransfer = 12,
}

const fn t_ms(code: u32) -> u64 {
    600 + 500 * code as u64
}
const WDT_OFFSET_MS: u64 = 150;
const EPATH_OFFSET_MS: u64 = 300;

#[repr(C, align(64))]
struct Dma {
    trb: [u32; 4],
    _pad: [u32; 12],
    buf: [u32; STATUS_WORDS],
}

#[repr(C)]
struct State {
    t0: u64,
    ticking: u32,
    stage: u32,
    in_fault: u32,
    el: u32,
    pbl_vbar: u64,
    loops: u64, // the time base when the counter does not run
}

// All state in a PROGBITS section, so the one LOAD segment has filesz == memsz: whether
// the boot ROM zero-fills memsz beyond filesz is unknown, so nothing relies on it.
#[unsafe(link_section = ".data.glassine")]
static mut DMA: Dma = Dma { trb: [0; 4], _pad: [0; 12], buf: [0; STATUS_WORDS] };
#[unsafe(link_section = ".data.glassine")]
static mut ST: State =
    State { t0: 0, ticking: 0, stage: 0, in_fault: 0, el: 0, pbl_vbar: 0, loops: 0 };

global_asm!(
    r#"
    .section .text.entry, "ax"
    .global _start
_start:
    mrs     x20, daif           // as the boot ROM left it
    msr     daifset, #0xf
    mov     x22, sp             // the boot ROM's stack and return address, for the record
    mov     x23, x30
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
    mrs     x21, cntpct_el0     // T0, read with our vectors in place (as B's main did)
    mov     x2, x19             // = TRAMPOLINE_MAGIC when entered through the XBL_SEC trampoline
    mov     x3, x20
    mov     x4, x21
    mov     x5, x22
    mov     x6, x23
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

fn st() -> *mut State {
    &raw mut ST
}

fn ticking() -> bool {
    unsafe { read_volatile(&raw const (*st()).ticking) != 0 }
}

/// Milliseconds since entry. When the counter does not run, counts calls instead
/// (~1 us each, assumed: rough, only bounds the waits); the watchdog then carries the
/// timing.
fn ms() -> u64 {
    unsafe {
        let s = st();
        if ticking() {
            counter().wrapping_sub((*s).t0) / TICKS_PER_MS
        } else {
            (*s).loops += 1;
            (*s).loops / 1000
        }
    }
}

fn wait_until_ms(t: u64) {
    while ms() < t {
        core::hint::spin_loop();
    }
}

fn delay_ms(d: u64) {
    let end = ms() + d;
    wait_until_ms(end);
}

/// Arm the APSS watchdog to bite at `at` ms after entry (qcom-wdt.c qcom_wdt_start order).
/// Without a running counter "now" is taken as 0: all markers but the send wait come
/// within milliseconds of entry. A no-op with the `no-wdt` feature (if probe R shows the
/// watchdog's registers fault).
fn wdt_bite_at(at: u64) {
    if cfg!(feature = "no-wdt") {
        return;
    }
    let now = if ticking() { ms() } else { 0 };
    let ticks = (at.saturating_sub(now).max(5) * WDT_HZ / 1000) as u32;
    wr(APSS_WDT + WDT_EN, 0);
    wr(APSS_WDT + WDT_RST, 1);
    wr(APSS_WDT + WDT_BARK_TIME, 0x0fff_ffff); // never: FIQ is masked anyway
    wr(APSS_WDT + WDT_BITE_TIME, ticks);
    wr(APSS_WDT + WDT_EN, 1);
    unsafe { asm!("dsb sy") };
}

/// Pass a stage marker: remember it and arm the watchdog for its time.
fn mark(c: Code) {
    unsafe { write_volatile(&raw mut (*st()).stage, c as u32) };
    wdt_bite_at(t_ms(c as u32) + WDT_OFFSET_MS);
}

/// Give the CPU back to the boot ROM's vectors with nothing masked: an exception or
/// interrupt there resets the chip (probe E's behaviour; probe U checks it).
fn e_path() -> ! {
    unsafe {
        let s = st();
        let vbar = read_volatile(&raw const (*s).pbl_vbar);
        match read_volatile(&raw const (*s).el) {
            3 => asm!("msr vbar_el3, {}", in(reg) vbar),
            2 => asm!("msr vbar_el2, {}", in(reg) vbar),
            1 => asm!("msr vbar_el1, {}", in(reg) vbar),
            _ => {}
        }
        asm!("isb", "msr daifclr, #0xf", "isb");
        loop {
            core::hint::spin_loop();
        }
    }
}

/// Stop with stage code `c`: reset at T(c) through the chain described at the top.
fn finish(c: Code) -> ! {
    mark(c);
    if !ticking() {
        // No counter, so no timed chain: the watchdog alone says where we stopped.
        loop {
            unsafe { asm!("wfe") };
        }
    }
    let t = t_ms(c as u32);
    wait_until_ms(t);
    // From here a fault goes straight to the E path, still on time.
    unsafe { write_volatile(&raw mut (*st()).in_fault, 1) };
    // PS_HOLD low: the PMIC resets or powers off, whichever its PS_HOLD config says.
    // drivers/power/reset/msm-poweroff.c.
    wr(PS_HOLD, 0);
    unsafe { asm!("dsb sy") };
    wait_until_ms(t + EPATH_OFFSET_MS); // the watchdog bites at t + 0.15 s on the way
    e_path()
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
/// None on timeout. (Before 2026-11-13 the parameter and command writes here lacked the
/// DWC3 base: the command went to 0xc8xx and never reached the controller.)
fn ep_command(ep: usize, cmd: u32, p0: u32, p1: u32) -> Option<u32> {
    wr(depcmdpar2(ep), 0);
    wr(depcmdpar1(ep), p1);
    wr(depcmdpar0(ep), p0);
    wr(depcmd(ep), cmd | DEPCMD_CMDACT);
    let end = ms() + 50;
    loop {
        let v = rd(depcmd(ep));
        if v & DEPCMD_CMDACT == 0 {
            return Some(v);
        }
        if ms() > end {
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
fn send_status(ep: usize, flags: &mut u32) -> Code {
    let dma = &raw mut DMA;
    let trb = unsafe { (&raw mut (*dma).trb).cast::<u32>() };
    let buf = unsafe { (&raw mut (*dma).buf) as usize };
    let trb_addr = trb as usize;
    let len = (STATUS_WORDS * 4) as u32;

    // drivers/usb/dwc3/gadget.c, dwc3_send_gadget_ep_cmd(): at USB 2.0 speeds GUSB2PHYCFG
    // SUSPHY and ENBLSLPM must be clear while an endpoint command runs (databook
    // 6.3.2.5); sdm630.dtsi sets snps,dis_u2_susphy_quirk + snps,dis_enblslpm_quirk for
    // this controller, so Linux keeps both off for good. Same here: never restored.
    let phy = rd(DWC3 + GUSB2PHYCFG0);
    if phy & (GUSB2PHYCFG_SUSPHY | GUSB2PHYCFG_ENBLSLPM) != 0 {
        wr(DWC3 + GUSB2PHYCFG0, phy & !(GUSB2PHYCFG_SUSPHY | GUSB2PHYCFG_ENBLSLPM));
        *flags |= F_PHY_SUSPEND_CLEARED;
    }

    let before = rd(depcmd(ep));
    put(24, before);

    mark(Code::Starting);
    for attempt in 0..2u32 {
        put(5, *flags);
        put(25, attempt);
        put(42, Code::NotTaken as u32);
        put(43, ms() as u32);
        put(44, rd(DWC3 + GEVNTCOUNT0));
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
            // The boot ROM may still own a transfer on this endpoint: end it (forced, no
            // completion event, so no wait on CMDACT), give the controller 1 ms as
            // gadget.c __dwc3_stop_active_transfer() does without CMDIOC, start again.
            put(26, r.unwrap_or(0xffff_ffff));
            *flags |= F_RETRIED;
            wr(depcmdpar2(ep), 0);
            wr(depcmdpar1(ep), 0);
            wr(depcmdpar0(ep), 0);
            wr(
                depcmd(ep),
                DEPCMD_ENDTRANSFER
                    | DEPCMD_HIPRI_FORCERM
                    | DEPCMD_CMDACT
                    | (depcmd_rsc_idx(before) << 16),
            );
            delay_ms(1);
            continue;
        }

        mark(Code::NotTaken);
        let end = ms() + SEND_TIMEOUT_MS;
        while ms() < end {
            drain_events();
            dc_civac(trb_addr, 16);
            if unsafe { read_volatile(trb.add(3)) } & TRB_HWO == 0 {
                return Code::Sent;
            }
        }
        return Code::NotTaken;
    }
    Code::StartFailed
}

#[unsafe(no_mangle)]
extern "C" fn glassine_main(
    el: u64,
    vbar_old: u64,
    via: u64,
    daif: u64,
    t0: u64,
    pbl_sp: u64,
    pbl_lr: u64,
) -> ! {
    unsafe {
        let s = st();
        (*s).t0 = t0;
        (*s).el = el as u32;
        (*s).pbl_vbar = vbar_old;
        (*s).stage = 0;
        (*s).in_fault = 0;
        (*s).loops = 0;
        (*s).ticking = 0;
    }
    let (wdt_en, wdt_bite) = if cfg!(feature = "no-wdt") {
        (0xffff_ffff, 0xffff_ffff)
    } else {
        (rd(APSS_WDT + WDT_EN), rd(APSS_WDT + WDT_BITE_TIME))
    };
    mark(Code::Entered);

    let t = counter();
    let mut spin = 0u32;
    while spin < 100_000 {
        spin += 1;
        core::hint::spin_loop();
    }
    let ticks = counter() != t;
    unsafe { write_volatile(&raw mut (*st()).ticking, ticks as u32) };
    let mut flags = if ticks { F_COUNTER_TICKS } else { 0 };
    if via == TRAMPOLINE_MAGIC {
        flags |= F_VIA_XBL_SEC;
    }

    let sctlr = el_sctlr(el);
    let (midr, mpidr, cntfrq): (u64, u64, u64);
    unsafe {
        asm!("mrs {}, midr_el1", out(reg) midr);
        asm!("mrs {}, mpidr_el1", out(reg) mpidr);
        asm!("mrs {}, cntfrq_el0", out(reg) cntfrq);
    }

    put(0, u32::from_le_bytes(*b"GLSN"));
    put(1, u32::from_le_bytes(*b"PRG0"));
    put(2, 2); // record version
    put(3, (STATUS_WORDS * 4) as u32);
    put(4, el as u32);
    put(5, flags);
    put64(6, midr);
    put64(8, mpidr);
    put64(10, sctlr);
    put64(12, vbar_old);
    put64(14, daif);
    put64(16, cntfrq); // raw, not used
    put(31, glassine_main as *const () as usize as u32);
    put64(32, pbl_sp);
    put64(34, pbl_lr);
    put(39, wdt_en);
    put(40, wdt_bite);
    put64(45, t0);
    mark(Code::CpuState);

    let id = rd(DWC3 + GSNPSID);
    put(18, id);
    mark(Code::Dwc3Read);
    put(19, rd(DWC3 + DCFG));
    let dctl = rd(DWC3 + DCTL);
    put(20, dctl);
    let dsts = rd(DWC3 + DSTS);
    put(21, dsts);
    let enabled = rd(DWC3 + DALEPENA);
    put(22, enabled);
    put(27, rd(DWC3 + GEVNTADRLO0));
    put(28, rd(DWC3 + GEVNTADRHI0));
    put(29, rd(DWC3 + GEVNTSIZ0));
    put(30, rd(DWC3 + GEVNTCOUNT0));
    put(36, rd(DWC3 + GUSB2PHYCFG0));
    put(37, rd(DWC3 + GCTL));
    put(38, rd(DWC3 + DEVTEN));

    // core.h DWC3_GSNPSID_MASK 0xffff0000: 0x5533 ("U3") for DWC3, 0x3331/0x3332 for DWC3.x.
    if !matches!(id >> 16, 0x5533 | 0x3331 | 0x3332) {
        finish(Code::NotDwc3);
    }
    if dctl & DCTL_RUN_STOP == 0 || dsts & DSTS_DEVCTRLHLT != 0 {
        finish(Code::NotRunning);
    }
    // Physical endpoint n: n = 2 * number + direction; IN endpoints are odd, ep0 is 0/1.
    let Some(ep) = (3..32).step_by(2).find(|n| enabled & (1 << n) != 0) else {
        finish(Code::NoEndpoint);
    };
    flags |= F_EP_FOUND;
    put(23, ep as u32);
    let c = send_status(ep, &mut flags);
    finish(c)
}

fn fault() -> ! {
    unsafe {
        let s = st();
        if read_volatile(&raw const (*s).in_fault) != 0 {
            // A fault inside the fault path: straight to the boot ROM (off the grid).
            e_path();
        }
        write_volatile(&raw mut (*s).in_fault, 1);
        let c = match read_volatile(&raw const (*s).stage) {
            0..=1 => Code::FaultEarly,
            2..=5 => Code::FaultSetup,
            _ => Code::FaultTransfer,
        };
        finish(c)
    }
}

#[unsafe(no_mangle)]
extern "C" fn glassine_exception(_vector: u64) -> ! {
    fault()
}

#[panic_handler]
fn panic(_: &PanicInfo) -> ! {
    fault()
}
