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
   endpoint addresses (`bEndpointAddress`). The status image takes the lowest enabled IN
   endpoint other than ep0 (physical ≥ 3); with one IN endpoint (expected: `0x81`) that is
   the right one. If the interface has more than one IN endpoint, note it: a code 8 then
   may mean the record went to the wrong one (word 23 / DALEPENA in a later record says
   which was used). Reading the cached descriptors sends nothing to the device.
4. Every reset so far landed in a fresh 9008 whose HELLO waited for minutes, so the runs
   can follow each other without a power hold. A hang (no reset within 20 s) or a 900E
   needs a long press on power (~10 s), then EDL entry again.

## Images for this session

| image | sha256 | what it is |
|---|---|---|
| `programmer-u-v5.mbn` | `904d967d849980a4092b9952d27780b90909ebca27dc702aab12be835d7fe197` | probe U: mask, wait 1.0 s (counter), unmask, spin |
| `programmer-r-v5.mbn` | `0a1e607cb39b331e26e567796a57f50d143d32a3e72134f32412ddcf11297e3c` | probe R: PS_HOLD at 0.5 s, watchdog bite 0.4 s armed at 0.8 s, unmask at 2.0 s |
| `programmer-s-v5.mbn` | `06e87d4834dcaa0ca8bd74e30ddfa2584850d3c086ccd7822ede8af8ad0f5a75` | status image: B's trampoline (same 32 bytes) + the fixed main; record on bulk IN, then a reset at the stage code's time |
| `programmer-n-v5.mbn` | `00e281ae3ea0bbff0cc9d3acb2938fcdc29d9d334aa3cf8f28d382fcbb23d7b8` | the status image without any watchdog access (instead of S if R shows the watchdog faults) |
| `programmer-g-v5.mbn` | `451609aae0837c016eb186424bcd3fe822897dfe8b19465c35fa9488e906e01f` | probe G: mask, spin forever (no counter, no MMIO) |

(Also built, not for this session: `programmer-a-v5.mbn`, `programmer-a-v3.mbn`, and E/D/F
again: their XBL_SEC bytes are the ones that ran (checked at this commit), the main code
inside them is the new one and is never reached. `build.sh` no longer emits a
`programmer-b`: the B that ran exists only at `738f4b0`; its successor is S.)

## Run order

One image per 9008 session. Write down X (the client's `ended` time) for each.

### Run 1: probe U

No MMIO, no main; tests the masking theory, the counter rate and the E path.

| X | means | next |
|---|---|---|
| ~0.004 s | masking is not what keeps B alive, **or** reading the counter faults (U's first instruction after the mask) | run G next (no counter: breaks the tie), then run 3 |
| **~1.0 s** (0.95–1.05) | masking keeps it alive; unmasking resets (the E path); the counter runs at 19.2 MHz | run 2 |
| other X < 7 s | as above, but the counter runs at 19.2 MHz / X | run 2; all timings below scale by X |
| ~7.56 s | masking keeps it alive; unmasking does not reset or the counter is ≥ 7× slow; 7.56 s is not our code | run 2, then 3 |
| no failure in 20 s | the counter does not run, or unmasking is harmless and 7.56 s was our code | power hold; run 3 (its watchdog markers work without the counter) |

### Run 2: probe R

Needs U's E path and counter. The watchdog is first touched at 0.8 s, so a fault there
can't hide the PS_HOLD result.

| X | means | then run 3 with |
|---|---|---|
| 0.5 s | **PS_HOLD low resets** (the watchdog stays untested) | S |
| 0.8 s | PS_HOLD does nothing; touching the watchdog faults | **N** (no watchdog) |
| 1.2 s | PS_HOLD does nothing; **the watchdog bite resets**; it was off before (note: 9008 or 900E afterwards?) | S |
| 1.4 s | as 1.2 s, but the boot ROM had left the watchdog **on**: prime suspect for B's 7.56 s | S |
| 2.0 s | neither resets; the E path does | S |
| ~0.004 s | should not happen after U ~1.0 s (same instructions up to 0.5 s) | S |

### Run 3: the status image

Expected on success: within ~0.1 s a 192-byte record on bulk IN starting `GLSNPRG0`
(record version 2, table below), then the bulk IN fails at **5.1 s** (code 9).

Otherwise X gives the stage code: **T(code) = 0.6 + 0.5 × code**, plus the mechanism
offset: **+0** PS_HOLD, **+0.15** watchdog, **+0.3** E path (run 2 says which ones work;
N has no +0.15 and no marker readings).
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
| 31 | address of `glassine_main` (sanity: S `0x14016610`, N `0x14016534`) |
| 32–33, 34–35 | the boot ROM's SP and x30 at the hand-over |
| 36, 37, 38 | GUSB2PHYCFG(0), GCTL, DEVTEN |
| 39, 40 | APSS watchdog WDT_EN, WDT_BITE_TIME at entry (N: `0xffffffff`) |
| 41 | 0 |
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

## Device run 2 (2026-11-13, session 68): U and S both hang at entry, no END_IMAGE_TX

| image | sha256 | what happened |
|---|---|---|
| `programmer-u-v5.mbn` | `904d967d…` | boot ROM read all segments (to file off 0x7a00, last READ_DATA_64 off 0x7000 len 0xa00); then **silence: no END_IMAGE_TX, no reset, 9008 stays** for 20 s+ (client times out waiting for END_IMAGE_TX). |
| `programmer-s-v5.mbn` | `06e87d48…` | **byte-for-byte the same trace as U**: all segments read, no END_IMAGE_TX, no record on bulk IN, no reset in 20 s, 9008 stays. |

Key contrast with B (738f4b0): B reached END_IMAGE_TX 0 + DONE_RESP **complete**, then ran
and reset at T0+7.56 s. U and S never reach END_IMAGE_TX — the hang is **before** the boot
ROM finishes the image transfer, i.e. at/just after the jump into XBL_SEC, in code common
to both images (trampoline / `_start` entry), not in either `main`. Both recovered with a
~10 s power hold → GlassineOS. No counter tick evidenced; no working watchdog reached.
Reads: client `glassine-install emergency-try-program` built from installer `origin/main`.

## Device run 3 (prepared): find a signal that works from masked code

Run 2 left us with no proven way for masked code to say "I got here": U/S hung silently, the
watchdog did not bite, the counter may be frozen. These three probes each test one
deliberate reset mechanism, counter-free, one image per 9008 session. Same harness as before
(try-run.sh with the lock + peer announce). Measurement: the client's bulk-IN read fails at
**< 20 s** and a fresh 9008 re-enumerates (the USB watch shows it) = the device reset; the
full 20 s timeout with 9008 still present = a hang (long-press power ~10 s to leave). None of
these reach END_IMAGE_TX (expected, as with U/S).

| order | image | sha256 | what a reset tells us |
|---|---|---|---|
| 1 | `programmer-p-v5.mbn` | `5708b42bcc539470…` | PS_HOLD low resets from behind the mask -> use PS_HOLD as the ACK. (Device powers off instead = also a usable, manual signal; nothing in 20 s = the store was lost.) |
| 2 | `programmer-m-v5.mbn` | `f496276795c08665…` | unmasking resets (~4 ms) -> unmask is the ACK, and U hung in its CNTPCT wait (counter frozen). Nothing in 20 s = unmask is not a signal here. |
| 3 | `programmer-k-v5.mbn` | `1d23c792377fe618…` | the APSS watchdog bites (~0.3 s) -> it works, so S hung before arming one (before/at glassine_main). ~0 ms = the WDT access faults; nothing in 20 s = the watchdog is dead here. |

Stop after the first clean reset: that mechanism becomes the ACK. Then (host work) a
bisection set inserts that ACK at points along _start -> glassine_main -> first DWC3 access,
to find exactly where S's main hangs. If none of P/M/K resets, no masked-code signal exists
at this stage and the status image must instead run with interrupts left unmasked (accept the
~4 ms reset) and lean entirely on the USB record for output.

### Device run 3 results (session 68)
- **P (PS_HOLD, masked):** same trace as U/S — all segments read, no END_IMAGE_TX, **no
  reset in 20 s**, 9008 stays. PS_HOLD low from masked code does nothing here (no reset, no
  power-off). Corollary: probe D (same write, unmasked) reset in 4 ms, so D's reset was the
  unmasked interrupt, not PS_HOLD. PS_HOLD is not a usable ACK. [Open hypothesis: P's only
  act was an MMIO store, and it was inert — our MMIO writes may not land at this stage, which
  would also explain S (no watchdog bite, no USB record). M (no MMIO) tests this.]
- **M (mask then unmask, no MMIO):** same as U/S/P — no END_IMAGE_TX, **no reset in 20 s**,
  9008 stays. Unmasking does NOT reproduce E/D/F's 4 ms reset → unmask is not an ACK, and
  the interrupt theory for E/D/F is in doubt. M does no MMIO, so this is not an "MMIO does
  not land" effect. Next: re-run E (byte-identical to the morning run that reset at 4 ms) as
  a control — does the apparatus still produce a reset at all today?
- **E (control):** HANGS too — but E here is a REBUILT image (`bd...`/then `56b8b1f2`), NOT byte-identical to the morning E (`ae533a34`). This pointed to a BUILD change, not the device: see RESEARCH §8.5. The morning image re-run later DID complete the handshake, proving the device is fine and the evening build regressed the layout.
### Device run 3 (rebuilt after the layout fix) — supersedes the voided run above

LOAD1 is now padded to the proven-good 0x5680 (link-xblsec.ld), so every image reproduces the
morning layout (XBL_SEC at file 0x8000). New shas:

- VALIDATE FIRST: `programmer-e-v5.mbn` `bd5a0489…` — the same spin probe as morning E, now
  with the fixed layout. EXPECT: END_IMAGE_TX + DONE_RESP, bulk IN fails ~T0+4 ms, fresh 9008
  (i.e. the handshake completes = the layout fix works). If it still hangs, the fix is wrong —
  stop and rethink before running anything else.
- THEN (one image per session), now that EVERY evening run is void and B's 7.56 s is open again:
  * U `39f619fa…` and G `ec280c6d…` re-settle B's 7.56 s / the counter (RESEARCH §8.3, §8.6
    retracted);
  * the signal probes, stop at the first clean reset: P `cdbb243c…` (PS_HOLD), M `4ade4ca3…`
    (unmask), K `8640f747…` (watchdog).
- status image once a signal is found: S `8b5e5f9c…`, N `50b9d49b…`.

Earlier run-3 shas (p `5708b42b`, m `f4962767`, k `1d23c792`, s `06e87d48`, …) are VOID: built
before the layout fix, they all hang at the LOAD1 wall (RESEARCH §8.7).

### Device run 3 VALIDATION (session 68): the layout fix WORKS

Rebuilt E (bd5a0489) completed the Sahara handshake: LOAD1 read to file 0x7680, XBL_SEC read
at 0x8000, END_IMAGE_TX 0, DONE, DONE_RESP complete ("10 reads, 24728 bytes, done Complete").
The layout fix is confirmed; our code reaches the hand-over again.

NOTE: rebuilt E (unmasked spin, `b .`) reset at T0+7.562 s, not the 4 ms the morning E/D/F
showed — same XBL_SEC bytes, only LOAD1 content differs. So 4 ms vs 7.56 s is NOT simply
mask-dependent; 7.56 s looks like a FIXED external boot-ROM timeout, with a faster interrupt
path that sometimes wins (morning E/D/F). This bears on B's 7.56 s (now reopened): run U + G.

### Device run 3 results after the layout fix (session 68) — valid runs

All three completed the Sahara handshake (END_IMAGE_TX, DONE, DONE_RESP complete): the layout
fix holds. The device auto-reset to a fresh 9008 after each, so runs chained with no power
hold.

| image | XBL_SEC | reset | record? |
|---|---|---|---|
| E (bd5a0489) unmasked spin | `b .` | T0+7.562 s | n/a |
| G (ec280c6d) mask + spin | `msr daifset; b .` | T0+0.004 s | n/a |
| S (8b5e5f9c) trampoline + fixed main | `ldr;ldr;br _start` | T0+0.004 s (x2, consistent) | NONE |

Reset-timing table across ALL valid runs is self-contradictory on the mask:
  morning no-mask (E/D/F) = 4 ms, morning mask+main (B) = 7.56 s;
  evening no-mask (E) = 7.56 s, evening mask (G, S) = 4 ms.
=> 4 ms vs 7.56 s is NOT caused by the interrupt mask; it is a host/device interaction (likely
a race on the client's first post-DONE_RESP bulk-IN poll vs an internal timeout), not our code.
B's 7.56 s is therefore NOT explained by its own code OR cleanly by an external timer; it is one
sample of the same racy reset. §8.3 stays open and the timing is not a reliable signal channel.

KEY RESULT: the fixed status image S hands over to main (our code runs) but the SoC resets at
~4 ms, consistently, before main can emit the GLSNPRG0 record. New wall: keep the device alive
long enough (or emit the record fast enough) to get bytes out. The ~4 ms reset survives the
interrupt mask (S and G both mask), so it is not an IRQ/SError our DAIF can block — it is an
external reset (boot-ROM watchdog, or the host tearing the link down on the first failed poll).

Directions (host work, to discuss): (a) change the client to NOT poll bulk-IN immediately after
DONE_RESP (test whether the poll triggers the reset, giving main time); (b) have main reuse the
boot ROM's already-enumerated bulk-IN endpoint and queue a TRB immediately, emitting the record
in <4 ms instead of re-initialising DWC3; (c) find and pet/disable the boot-ROM watchdog.
