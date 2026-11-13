# Device test plan: the GlassineOS programmer on the boot ROM (issue #18 step 3)

**Gated:** only with the owner's go and the user at the device. Nothing in any image here
can write storage: there is no eMMC code. The images touch only their own RAM, the USB
controller (DWC3 at `0x0a800000`), PS_HOLD (`0x010ac000`) and the APSS watchdog
(`0x17817000`). Background and reasoning: RESEARCH.md §8.

## Done: the first uploads (2026-11-13)

Reproducible with `./build.sh` at commit `738f4b0` (stock qtestsign; byte-identical to
what was sent):

| image | sha256 | result |
|---|---|---|
| `programmer-a-v5.mbn` | `a18f96ac…` | refused, END_IMAGE_TX 0x28 |
| `programmer-b-v5.mbn` | `498046bf…` | accepted; no record; bulk IN pends, fails at T0 + 7.562 s, fresh 9008 |
| `programmer-e-v5.mbn` | `ae533a34…` | accepted; bulk IN fails at T0 + 4 ms, fresh 9008 |
| `programmer-d-v5.mbn` | `6680d872…` | same as E |
| `programmer-f-v5.mbn` | `b7f333b1…` | same as E |

## Before the next session

1. `./build.sh`; compare `out/SHA256SUMS` with the table below.
2. Host: client = `glassine-install emergency-try-program IMAGE` (PR #24, branch
   `programmer-try`). It logs every Sahara read, then reads bulk IN for 20 s and prints
   `bulk IN at T0+X ended: …` (X = **the measurement**, ±5 ms) or the record. Keep
   `journalctl -k -o short-precise -f` running: it shows the disconnect ~0.09–0.25 s after X
   and the new 9008 ~0.17–0.2 s after that (a check, not the measurement).
3. Once, with the unit in 9008 and before any upload: `lsusb -v -d 05c6:9008` and note the
   bulk-IN endpoint address (`bEndpointAddress`). The status image assumes `0x81`
   (physical endpoint 3). Reading the cached descriptors sends nothing to the device.
4. Every reset so far landed in a fresh 9008 whose HELLO waited for minutes, so the runs
   can follow each other without a power hold. A hang (no reset within 20 s) or a 900E
   needs a long press on power (~10 s), then EDL entry again.

## Images for this session

| image | sha256 | what it is |
|---|---|---|
| `programmer-u-v5.mbn` | `1a9d6852bb1bf4f482d69a90929ec262a32a99f79fd014135efb90050314a5b5` | probe U: mask, wait 1.0 s (counter), unmask, spin |
| `programmer-r-v5.mbn` | `2fc98d9d253c33c481750cefc571d789705abd41ad3ab9b9aab9ce8a3ffccb23` | probe R: PS_HOLD at 0.5 s, watchdog bite 0.4 s armed at 0.8 s, unmask at 2.0 s |
| `programmer-s-v5.mbn` | `e4a6c4c0932e345f2dd55f8e44a3e8ec9114ed7fe89b7cd7039b685371e23024` | status image: B's trampoline + the fixed main; record on bulk IN, then a reset at the stage code's time |
| `programmer-g-v5.mbn` | `2383dece7064fe77691d952163de18b535dd9cc37299a35a3c930c0bec162dc9` | probe G (optional): mask, spin forever |

(Also built, not for this session: `programmer-a-v5.mbn`, `programmer-a-v3.mbn`, and E/D/F
again; their XBL_SEC bytes are the ones that ran, the main code inside them is the new one
and is never reached.)

## Run order

One image per 9008 session. Write down X (the client's `ended` time) for each.

### Run 1: probe U

No MMIO, no main; tests the masking theory, the counter rate and the E path.

| X | means | next |
|---|---|---|
| ~0.004 s | masking is not what keeps B alive | skip run 2 (it would read the same); run 3 |
| **~1.0 s** (0.95–1.05) | masking keeps it alive; unmasking resets (the E path); the counter runs at 19.2 MHz | run 2 |
| other X < 7 s | as above, but the counter runs at 19.2 MHz / X | run 2; all timings below scale by X |
| ~7.56 s | masking keeps it alive; unmasking does not reset or the counter is ≥ 7× slow; 7.56 s is not our code | run 2, then 3 |
| no failure in 20 s | the counter does not run, or unmasking is harmless and 7.56 s was our code | power hold; run 3 (its watchdog markers work without the counter) |

### Run 2: probe R

Needs U's E path and counter. All times shift by +0.2 s if the APSS watchdog was already
enabled at entry (by the boot ROM).

| X | means |
|---|---|
| ~0.004 s | reading the watchdog's enable register faults (into the boot ROM's vectors) |
| 0.5 s / 0.7 s | **PS_HOLD low resets** (watchdog was off / on at entry) |
| 0.8 s / 1.0 s | PS_HOLD does nothing; writing the watchdog faults |
| 1.2 s / 1.4 s | PS_HOLD does nothing; **the watchdog bite resets** (note: 9008 or 900E afterwards?) |
| 2.0 s / 2.2 s | neither resets; the E path does |

A 0.7 / 1.0 / 1.4 / 2.2 s reading also says the boot ROM left the APSS watchdog running,
which makes it the prime suspect for B's 7.56 s.

### Run 3: the status image

Expected on success: within ~0.1 s a 192-byte record on bulk IN starting `GLSNPRG0`
(record version 2, table below), then the bulk IN fails at **5.1 s** (code 9).

Otherwise X gives the stage code: **T(code) = 0.6 + 0.5 × code**, plus the mechanism
offset: **+0** PS_HOLD, **+0.15** watchdog, **+0.3** E path (run 2 says which ones work).
Marker codes (0, 1, 2, 6) only ever show with the watchdog offset: they mean "hung right
after this marker".

| code | T | meaning |
|---|---|---|
| 0 | (0.75) | hung after reaching `glassine_main` (stack, vectors, mask in place) |
| 1 | (1.25) | hung after the CPU state was recorded and the counter checked |
| 2 | (1.75) | hung after the first DWC3 register read returned |
| 3 | 2.1 | GSNPSID is not a DWC3 ID: wrong controller at `0x0a800000` |
| 4 | 2.6 | controller not running (DCTL.RUN_STOP clear or DSTS.DEVCTRLHLT set) |
| 5 | 3.1 | no bulk-IN endpoint enabled (DALEPENA) |
| 6 | (3.75) | hung before or in STARTTRANSFER (a register access stalls) |
| 7 | 4.1 | STARTTRANSFER refused or timed out, also after ENDTRANSFER + retry |
| 8 | 4.6 | transfer started, but the TRB was not taken within 1 s (4.75 can also mean a hang in that wait) |
| 9 | 5.1 | TRB taken: the record should have arrived |
| 10 | 5.6 | exception or panic before any DWC3 access |
| 11 | 6.1 | exception or panic in the DWC3 setup (codes 2–5) |
| 12 | 6.6 | exception or panic in the transfer (6 on) |

Off the grid: ~0 s = a fault inside the fault path (e.g. the watchdog registers fault:
compare run 2); ~7.56 s = the external reset, our code stopped before its first marker or
the watchdog never works; nothing in 20 s = hung with no working watchdog (power hold).

Without a running counter (run 1 "no failure"), only the watchdog timings appear, and
only approximately (each marker arms it as if it came at T0).

### Run 4 (only if still open): probe G

Mask and spin, nothing else. Run it only if runs 1–3 leave open where B's 7.56 s came from
(e.g. run 1 gave ~1.0 s and run 3 ended on the grid). X ≈ 7.56 s: an external timer;
nothing in 20 s: it was our old code (power hold).

## Status record, version 2 (little-endian u32 words, 192 bytes)

| Word | Field |
|---|---|
| 0–1 | `GLSN` `PRG0` |
| 2, 3 | record version 2, length 192 |
| 4 | exception level at entry |
| 5 | flags: bit0 counter ticks, bit1 bulk-IN found, bit2 START timed out, bit3 END + retry, bit4 entered through the XBL_SEC trampoline, bit5 GUSB2PHYCFG SUSPHY/ENBLSLPM were set and got cleared |
| 6–7, 8–9 | MIDR_EL1, MPIDR_EL1 |
| 10–11 | SCTLR_ELx at entry (MMU/caches) |
| 12–13 | VBAR_ELx the boot ROM had (its vector table) |
| 14–15 | DAIF as the boot ROM left it (read before masking) |
| 16–17 | CNTFRQ_EL0 as found (not used; the timebase is a fixed 19.2 MHz) |
| 18–22 | DWC3 GSNPSID, DCFG, DCTL, DSTS, DALEPENA |
| 23 | physical bulk-IN endpoint used |
| 24 | its DEPCMD before our command (the boot ROM's last command, resource index) |
| 25, 26 | attempt number; DEPCMD of a failed START |
| 27–30 | event buffer 0: address lo/hi, size, count (where the boot ROM keeps it) |
| 31 | address of `glassine_main` (sanity: `0x14016618`) |
| 32–33, 34–35 | the boot ROM's SP and x30 at the hand-over |
| 36, 37, 38 | GUSB2PHYCFG(0), GCTL, DEVTEN |
| 39, 40 | APSS watchdog WDT_EN, WDT_BITE_TIME at entry |
| 41 | DEPCMD after the START that worked (resource index) |
| 42, 43, 44 | stage code at send time (8), ms since entry, GEVNTCOUNT before the START |
| 45–46 | CNTPCT_EL0 at entry |
| 47 | 0 |

## Failure and escape

- **Refusal in Sahara:** harmless; the boot ROM stays in Sahara. Sahara RESET or power hold.
- **No failure within 20 s:** a hang with USB still up. Long-press power (~10 s).
- **900E after a reset:** the watchdog path landed in crash-dump mode. Long-press power.
- **Device vanished, nothing comes back:** PS_HOLD may have powered it off; press power.
  If the power key does nothing: the EDL cable / battery-drain route the lab documented.
- After the session: Android (or the installed system) must boot unchanged; the images
  have no write path.

## Not part of this test

eMMC access, any Firehose command, any write. Re-enumeration with our own VID/PID.
