# A GlassineOS programmer for the SDM636 boot ROM: research

Spike for [installer#18](https://github.com/GlassineOS/installer/issues/18) step 1 and
[spec 004](../../specs/004-emergency-mode/spec.md) (branch `emergency-mode`). Sections 1–7
were written before any device run; the first uploads (2026-11-13, issue #18) are in §8,
which supersedes the older text where they disagree.

Target: Onyx Boox Note Air (SDM636, sdm660 family, eMMC). The boot ROM (PBL) in its
emergency mode enumerates as `05c6:9008` and loads one ELF image over the Sahara protocol;
vendors ship a "Firehose programmer" there. We want our own.

Files: the draft image is `src/main.rs` (with `link.ld` / `link-xblsec.ld`), built and
test-signed by `./build.sh` (Docker, pinned toolchain, offline build) into the git-ignored
`out/`; the gated device step is `TESTPLAN.md`.

**Labels used below.** Every claim carries one:

- **[src]** read in a public source named next to it.
- **[lab]** observed on this device model in the lab (its notes, not public).
- **[layout]** read from the program headers of the vendor programmer the lab uses, and
  nothing else: no code, no data, no hash segment content. No byte of it is in this repo.
- **[inferred]** follows from the above, not observed.
- **[unknown]** open; the device test or more research has to answer it.
- **[device]** measured on the lab unit in the uploads of 2026-11-13 (§8).

## 1. Will the PBL run an image we sign ourselves?

- [device] **Yes**, with an XBL_SEC-typed segment: variant B (qtestsign `prog`, v5) was
  accepted and run on 2026-11-13 (§8). The unfused inference below holds.

- [lab] fastboot reports `secure: no`. In ABL that value comes from the closed XBL protocol
  `VBIsDeviceSecure` (ABL source, `QcomModulePkg/Library/BootLib/LinuxLoaderLib.c`,
  `IsSecureBootEnabled()`; published by Qualcomm on CodeLinaro, `clo/la/abl/tianocore/edk2`),
  so it is XBL's view of the secure-boot fuse, not a direct fuse read.
- [lab] The stock Onyx ABL is signed with qtestsign's "Generated Test Attestation CA", and
  our own ABL signed with qtestsign `-v 5` boots; `-v 3` is silently refused by XBL.
- [inferred] XBL authenticates with the same fuse-driven secure-boot setting the PBL uses,
  so the PBL most likely checks only the hashes (hash segment) and not the signature. The
  usual Qualcomm model (whitepaper "Secure Boot and Image Authentication", v1.0/v2.0,
  linked from the [qtestsign README](https://github.com/msm8916-mainline/qtestsign)) is:
  fuse off → images still need a well-formed hash segment, signature not checked. **Not
  proven for the PBL**; spec 004 scenario 2 (Sahara command mode, read OEM_PK_HASH and
  MSM_HW_ID, issue step 2) is the read-only check before any image is sent.
- [src] qtestsign generates the dummy chain from fixed keys and fixed dates
  (`mbn/cert.py`), and RSA PKCS#1 v1.5 is deterministic, so the signed image is
  reproducible (SC-002). Confirmed: two builds give identical hashes.

### MBN header version: v5

- [src] qtestsign README: `-v5` for MSM8998 and SDM845; `-v3` for MSM8916/8939/8953.
  SDM660 is the MSM8998 generation (Kryo 2xx).
- [lab] XBL on this device needs v5 for ABL (v3 refused).
- [src] Qualcomm's own `createxbl.py` gained "extended MBNV5" for images that carry an
  XBL_SEC segment (coreboot commit
  [101098c4](https://github.com/coreboot/coreboot/commit/101098c41a047184e3eceabca2c1baa11141f36e),
  "sdm845: Combine BB with QC-Sec for ROM boot", `util/qualcomm/createxbl.py`).
- **Decision: v5.** A v3 image is also built (`programmer-a-v3.mbn`): a refusal by the PBL
  is a harmless Sahara error, so it is a cheap comparison if v5 is refused. [inferred] The
  "extended" v5 variant is not produced by qtestsign; if both are refused, that is the next
  thing to look at.

## 2. What the image must look like

From the vendor programmer the PBL accepts on this SoC [layout]:

| What | Value |
|---|---|
| ELF | ELF64, little endian, AArch64, `ET_EXEC`, no section headers |
| Entry | `0x140236c0` (inside the first LOAD segment) |
| phdr 0 | `NULL`, offset 0, filesz 0x318, flags `0x07000000` (covers ELF + program headers) |
| phdr 1 | `NULL` at file offset 0x1000, flags `0x02200000` = the hash segment (vaddr in DDR, `0x85e28000`, memsz 0x2000) |
| LOAD, on-chip | `0x14015000`–`0x140b08d8` in several segments (RX, RW, RWX) |
| LOAD, on-chip | `0x14680000` (bss only), `0x146ae000` (RX, 0x1e7c), `0x146b1000`/`0x146b2000` (bss only) |
| LOAD, DDR | `0x85e10000`, **filesz 0** (bss only) |
| LOAD, XBL_SEC | `0x14699000`, RX, filesz 0x12dc0, flags **`0x05000005`** |

What follows from it:

- [src] Segment type bits are `p_flags & 0x07000000 >> 24`: 7 = phdr placeholder, 2 = hash,
  **5 = XBL_SEC** (coreboot `util/qualcomm/mbn_tools.py`, `MI_PBT_*_SEGMENT`; same flags in
  qtestsign `mbn/hashseg.py`). createxbl.py writes exactly `0x5 | (5 << 24)` for the
  embedded QC-Sec ELF.
- [inferred] **No segment with file content lies in DDR.** The DDR segment has filesz 0 and
  the hash segment's vaddr is in DDR, so the PBL either ignores that vaddr or DDR is never
  written before the programmer runs. The programmer (file name `..._ddr.elf`) brings its
  own DDR init. So: **the PBL does not initialise DDR**, and our image stays in on-chip RAM.
- [inferred] Every range the PBL writes with file content while it is still running Sahara
  (it writes segment by segment over USB) cannot hold the PBL's own live state. So
  `0x14015000`–`0x1406d9c4` is safe to load into. Our image uses `0x14015000`–`0x1401a680`
  (22 KiB, code + 256 B state + 16 KiB stack), hash segment vaddr `0x1401b000` (variant A).
- [unknown] Whether the PBL zero-fills memsz beyond filesz. Our segment has
  filesz == memsz, so it does not matter.
- [unknown] The name and full extent of the on-chip RAM at `0x14000000`. Mainline only
  describes `sram@146bf000` (4 KiB "imem", `qcom,sdm630-imem`, holds `pil-reloc` info) in
  `arch/arm64/boot/dts/qcom/sdm630.dtsi`.

### The XBL_SEC question (the biggest unknown)

- [layout] The accepted programmer carries an XBL_SEC segment, the same size
  (0x12dc0) in both copies the lab has (OEM and "peek"), at `0x14699000`.
- [src] On the same generation, coreboot builds its ROM-booted bootblock by merging it with
  Qualcomm's `qc_sec.mbn` (QC-Sec = XBL_SEC): sc7180
  ([`src/soc/qualcomm/sc7180/Makefile.mk`](https://github.com/coreboot/coreboot/blob/main/src/soc/qualcomm/sc7180/Makefile.mk):
  `createxbl.py --mbn_version 6 -f bootblock.raw.elf -x qc_sec.mbn ... -a 64 -d 64 -c 64`)
  and earlier sdm845/qcs405 (commit 101098c4).
- [src] Aleph Security found the OnePlus 5 (MSM8998, same generation) programmer running at
  **EL1**, `VBAR_EL1 = 0x14015000`, `TTBR0_EL1 = 0x1400f000`, `SCTLR_EL1 = 0x00c00800`
  ([part 3](https://alephsecurity.com/2018/01/22/qualcomm-edl-3/)), while the older
  aarch64 Nexus 6P (MSM8994) programmer ran at EL3. All PBLs they extracted ran in AArch32
  ([part 1](https://alephsecurity.com/2018/01/22/qualcomm-edl-1/)); SDM660 is listed there
  only as a programmer, not as an extracted PBL.
- [inferred] On this generation the PBL hands control to the XBL_SEC part at EL3, which sets
  up the secure monitor and enters the main image at EL1. That matches the OnePlus 5 EL1
  and the `0x14015000` base both share.
- [unknown] **What the PBL does with an image that has no XBL_SEC segment.** Possible:
  (a) jumps to `e_entry` at EL3 (older generations), (b) refuses it in Sahara (a NAK; harmless),
  (c) jumps into a missing XBL_SEC (hang; power key). A Qualcomm XBL_SEC is a vendor binary
  and can't be used (FR-002), and its PBL interface isn't public.
- **[device] Answered for the refusal:** without an XBL_SEC-typed segment the boot ROM
  refuses right after reading the program headers (END_IMAGE_TX status 0x28; also with
  the hash segment at the vendor's DDR address). With one (variant B) it reads the hash
  segment and every segment (the XBL_SEC one last), END_IMAGE_TX 0, DONE_RESP complete,
  and execution starts in the XBL_SEC segment (§8). So (b) above.
- **What the draft does about it:** two variants. **A** has one LOAD segment. **B** adds an
  XBL_SEC-typed segment at `0x14699000` holding only a jump to `_start` (and a marker in
  `x19`), so the image runs whichever entry the PBL takes. The status record says which
  one ran and at which EL. [unknown] Whether the PBL expects XBL_SEC to *return* to it
  (then B works by accident, as our code never returns) or checks its content beyond the
  hash.

## 3. CPU state at entry

- [src] AArch64 (the accepted programmer is ELF64 AArch64).
- [unknown] Exception level: EL3 or EL1, see above. The code handles EL1, EL2 and EL3: it
  reads `CurrentEL` and uses the matching `VBAR_ELx`/`SCTLR_ELx`.
- [unknown] MMU and caches. On the OnePlus 5 `SCTLR_EL1 = 0x00c00800` (M, C and I off) was
  read *while the programmer ran*, so it says what the programmer chose, not what the PBL
  handed over. The draft changes none of it: it records `SCTLR`, does `DC CIVAC` on the
  DMA buffers (correct with caches on or off), and every access is aligned
  (`aarch64-unknown-none-softfloat` has `+strict-align`: with the MMU off all data
  accesses are Device memory, where unaligned accesses fault).
- [unknown] Whether FP/SIMD is trapped. The softfloat target emits no FP/SIMD instruction
  (checked in the disassembly).
- [unknown] Interrupts: the draft masks DAIF first, so a PBL USB interrupt handler can't run
  and race us. It installs its own vectors; any exception soft-disconnects USB and resets.
- [unknown] Whether the generic timer counter runs. The draft checks that `CNTPCT_EL0`
  advances; if it doesn't, it falls back to counting loop iterations (rough timeline).
  [src] The sdm630 counter frequency is 19.2 MHz (`timer@17920000`, `xo_board`, sdm630.dtsi);
  used if `CNTFRQ_EL0` reads 0.

## 4. USB after the hand-over

(The first device run and the bug it exposed in the code below: §8.4.)

- [lab] In 9008 the device is a high-speed USB device `05c6:9008` with one bulk-IN and one
  bulk-OUT endpoint (Sahara runs on them).
- [src] The device's USB port is the DWC3 controller `usb3_dwc3: usb@a800000` with the
  QUSB2 PHY `qusb2phy0: phy@c012000` (+ QMP USB3 PHY at `0x0c010000`); the second
  controller `usb2: usb@c2f8800`/`usb@c200000` is a high-speed host only
  (`arch/arm64/boot/dts/qcom/sdm630.dtsi`). Our NA1 board DT (GlassineOS kernel,
  `sdm636-onyx-noteair.dts`) uses `usb3_dwc3` with `qusb2phy0` for the port.
- [inferred] So the PBL drives the same DWC3 at `0x0a800000` in high-speed device mode.
  ChipIdea is not on this SoC in mainline.
- [src] After Sahara `DONE`, bkerler's edl tool talks Firehose on the same open USB handle
  after a 0.3 s sleep, with no reconnect (`edl.py`, after `upload_loader`;
  [github.com/bkerler/edl](https://github.com/bkerler/edl)), and [lab] that works on this
  device. [inferred] So the vendor programmer continues on the PBL's enumerated device
  (a controller reset would drop the host's handle). Reusing it is possible in principle.
- **What the draft does** (DWC3 registers from `drivers/usb/dwc3/core.h`, usage as in
  `drivers/usb/dwc3/gadget.c`): reads `GSNPSID` (`0xc120`), `DCFG/DCTL/DSTS`
  (`0xc700/0xc704/0xc70c`), the event buffer 0 registers (`0xc400`–`0xc40c`) and
  `DALEPENA` (`0xc720`); takes the lowest enabled odd physical endpoint ≥ 3 as bulk IN
  (physical number = 2 × endpoint + direction); writes one normal TRB
  (`HWO|LST|TRBCTL_NORMAL|IOC`, 128 bytes, a short packet) and issues `DEPSTARTXFER`
  (`DEPCMDPAR0` = TRB address high, `PAR1` = low). If that fails (e.g. the PBL still owns
  a transfer on it), it issues `DEPENDXFER` with `HIPRI_FORCERM` and the resource index
  from the PBL's last `DEPCMD` value, then starts again. Completion is read from the TRB's
  HWO bit; the event buffer is drained (`GEVNTCOUNT` written back) without parsing.
- [unknown] Whether the PBL left the bulk-IN endpoint with a transfer in progress, whether
  its TRB/event buffers sit where our image was loaded (they can't, see §2, if it was still
  using them during the transfer), and whether the PBL's link power management lets the
  controller sleep before our transfer.
- [unknown] Re-enumerating with our own VID/PID would need ep0 (control) handling: our own
  descriptors, SET_ADDRESS, SET_CONFIGURATION, and a re-init of the device side. The draft
  doesn't: a soft-disconnect (`DCTL.RUN_STOP = 0`) after 5 s is its second, simpler proof
  (the host sees 9008 vanish at a time only our code would choose).

## 5. eMMC (not in the draft; for issue step 4)

- [src] `sdhc_1: mmc@c0c4000`, `qcom,sdm630-sdhci` / `qcom,sdhci-msm-v5`, `hc` at
  `0x0c0c4000`, `cqhci` at `0x0c0c5000`, `ice` at `0x0c0c8000`; 8-bit, non-removable
  (sdm630.dtsi). The vendor-specific registers are inside the `hc` block for v5 (no
  separate MCI block): `sdhci_msm_v5_offset` in `drivers/mmc/host/sdhci-msm.c`
  (`core_pwrctl_*` at `0x240`–`0x24c`, `core_vendor_spec` `0x20c`, `core_dll_config` `0x200`).
- [src] Clocks in GCC (`clock-controller@100000`, `0x00100000`, size `0x94000`),
  `drivers/clk/qcom/gcc-sdm660.c`: `gcc_sdcc1_apps_clk` branch `0x16004`,
  `gcc_sdcc1_ahb_clk` `0x16008` (enable = bit 0), RCG `sdcc1_apps_clk_src` cmd_rcgr
  `0x1602c` (400 kHz = XO, pre-divider 12, M/N 1/4; 25 MHz from GPLL0 early div),
  `GCC_SDCC1_BCR` `0x16000`. USB for comparison: `GCC_USB_30_BCR` `0xf000`, master branch
  `0xf008`, RCG `0xf014`.
- [unknown] What the PBL leaves on. [src] On the SoCs Aleph examined, software EDL is a bit
  in `tcsr-boot-misc-detect` that the PBL checks (part 1); [inferred] if the SDM636 PBL
  checks it before touching the boot device, SDCC1 was never initialised and may be off. The safe plan is a full
  init from XO at 400 kHz, then 25/50 MHz, PIO or ADMA into on-chip RAM. Rails (eMMC VCC/
  VCCQ) are presumably PMIC defaults on, since the PBL boots from eMMC with them [inferred].
- [inferred] Reading the whole disk (61 071 360 × 512-byte sectors [lab]) through on-chip
  RAM at high-speed USB is fine; DDR is not needed for a read-only programmer.

## 6. Reset and watchdog

(What the first device run showed about resets, and the probes that test PS_HOLD and the
watchdog: §8.)

- [src] PS_HOLD: `restart@10ac000`, `qcom,pshold` (sdm630.dtsi); the mainline driver
  writes 0 to it (`drivers/power/reset/msm-poweroff.c`). What then happens is the PMIC's
  PS_HOLD reset type (`PON_PS_HOLD_RST_CTL` `0x5a`: warm reset 1, shutdown 4, hard reset 7,
  `drivers/input/misc/pm8941-pwrkey.c`).
- [unknown] Which type the PMIC has when only the PBL has run (XBL normally configures
  it). It may reset or power off. Both end the emergency mode; a power-off needs the power
  key.
- [unknown] Whether the software-EDL request survives that reset (then the device comes
  back as 9008, which the host can see and then reset again from Sahara with `RESET`).
- [src] Second fallback: APSS watchdog `qcom,kpss-wdt` at `0x17817000` (NA1 board DT;
  stock DT `qcom,wdt@17817000`), registers `WDT_RST 0x4`, `WDT_EN 0x8`, `BARK 0x10`,
  `BITE 0x14`, 32 768 Hz sleep clock (`drivers/watchdog/qcom-wdt.c`,
  `reg_offset_data_kpss`). [lab] Crashes on this device tend to land in the 900E crash-dump
  mode, so a bite may end there; the power key still gets out.
- [unknown] Whether the PBL runs a watchdog while in 9008, and with which timeout. Our code
  never pets one: if one exists, it resets the device, which is the wanted end state
  (FR-005), unless it fires before the host has read the status record.
- MPM: [inferred] `0x010ac000` lies in the MPM2 block (older Qualcomm DTs call this register
  `mpm2_ps_hold`); nothing else from MPM is needed for a reset.

## 7. Sahara, as the host sees it

- [src] Commands (`drivers/accel/qaic/sahara.c` in mainline Linux; bkerler
  `edlclient/Library/sahara_defs.py`; [linux-msm/qdl](https://github.com/linux-msm/qdl)
  `sahara.c`): HELLO 0x01, HELLO_RESP 0x02, READ_DATA 0x03, END_OF_IMAGE 0x04, DONE 0x05,
  DONE_RESP 0x06, RESET 0x07, CMD_READY 0x0b, SWITCH_MODE 0x0c, EXECUTE 0x0d–0x0f,
  READ_DATA64 0x12 (protocol ≥ 2.8). Modes: IMAGE_TX_PENDING 0, IMAGE_TX_COMPLETE 1,
  COMMAND 3. Status 0 = success; 0x21 hash table auth failure, 0x22 hash verification
  failure, 0x23 hash table not found, 0x25 image auth failure, 0x12 invalid destination
  address, 0x14 invalid ELF header.
- [src] Command mode EXECUTE 0x02 = MSM_HW_ID, 0x03 = OEM_PK_HASH (no image sent).
- [lab] The vendor loader the lab uses is filed under `000cc0e1…_7be49b72f9e43372`
  (bkerler/Loaders naming: MSM HW ID, then the first bytes of the OEM PK hash of the unit it
  was taken from).

## 8. The first uploads (2026-11-13): what resets the chip, and why B sent nothing

Labels here: **[device]** measured (client log: Sahara reads and the bulk-IN result with
ms timestamps from T0 = DONE_RESP; host kernel log with µs timestamps), **[src]** read in
a public source or in our own code, **[inferred]**, **[unknown]**. Images: B, E, D, F as in
issue #18 (all reproducible at commit `738f4b0`, see TESTPLAN.md); C (hash segment moved)
is dropped: it changed nothing, and stock qtestsign reproduces every image that ran.

### 8.1 The "0.3 s" is not a timer

What the host saw after DONE_RESP (EPROTO = the client's pending bulk-IN read failing;
the absolute time of that failure is also where qcserial rebinds, as the client lets go
of the interface right after it):

| image | bulk IN fails (EPROTO) | kernel "USB disconnect" after that | new 9008 enumerated after that |
|---|---|---|---|
| B | T0 + 7.562 s | +87 ms | +194 ms |
| E (`b .`) | T0 + 0.004 s | +100 ms | +195 ms |
| D (PS_HOLD) | T0 + 0.004 s | +181 ms | +170 ms |
| F (`ret`) | T0 + 0.004 s | +251 ms | +167 ms |

- [device] B and E/D/F end with **the same signature**: the device stops answering
  (EPROTO), the hub reports the disconnect 87–251 ms later, a fresh 9008 enumerates
  ~170–195 ms after that. In E/D/F the device stopped answering at the first IN poll
  after DONE_RESP (4 ms). So the "~0.3 s" of the issue is reset + boot ROM restart +
  enumeration latency, **not a timer that fires at 0.3 s**.
- [inferred] **E, D and F reset the chip within ≤ 4 ms of the jump.** B resets at 7.56 s.
- [device] The disconnect lags the failure by a variable 87–251 ms (hub polling), so the
  **client's EPROTO time is the measurement** (≈ ±5 ms); the kernel log only confirms it
  (disconnect = EPROTO + ~0.1–0.25 s, re-enumeration ≈ EPROTO + 0.28–0.42 s).
- [device] n = 1 for B's 7.56 s: the earlier B attempt (14:24) never got a HELLO
  (host-side timing, not an upload), so there is no second sample.

### 8.2 The immediate reset in E, D, F

- [device] E, D, F differ from B only in the 32 bytes of the XBL_SEC segment, and B
  survives 7.56 s. So the boot ROM executes the XBL_SEC segment (it does not merely hash
  it), and `ret` (F) does not lead on to `e_entry`.
- [src] B's first five instructions are `ldr x19; ldr x16; br x16` (trampoline), then
  `mrs x20, daif; msr daifset, #0xf` in `_start`. E/D/F never mask.
- [inferred] Best candidate: the boot ROM hands over with interrupts (or SError) unmasked
  and its own vector table live; within milliseconds an interrupt (the USB controller has
  events to report: the host polls bulk IN right after DONE_RESP; or a timer) enters the
  boot ROM's handler outside Sahara, which resets into EDL. B masks within five
  instructions and survives. Not proven: an already-pending interrupt would have been taken
  before B's mask, so the interrupt must arrive after the first few instructions.
- [inferred] D cannot tell whether PS_HOLD resets: it resets as fast as E either way.
- [unknown] Whether a boot-ROM watchdog exists and with which timeout. No public source
  found describes a PBL watchdog during Sahara (searched: Aleph Security parts 1–5,
  Qualcomm's public system-debugging guide, QFIL/QPST notes; only generic "Sahara timeout"
  host-side errors).
- **Probe U** tests this: mask, wait 1.0 s on the counter, unmask, spin; no MMIO, the boot
  ROM's VBAR untouched. Reset at ~4 ms → masking is not the protection; at ~1.0 s →
  masking is, unmasking resets ("the E path"), and the counter runs at 19.2 MHz.

### 8.3 B's reset at 7.56 s: two live readings

What the old `main` did, by its own clock [src, `main.rs` at `738f4b0`]: status record
on bulk IN (≤ 4 s wait), soft disconnect (`DCTL.RUN_STOP = 0`) at 5 s, PS_HOLD at 15 s,
watchdog bite 0.1 s after 17 s. Its clock: `CNTPCT_EL0` divided by `CNTFRQ_EL0 / 1000`
(19.2 MHz if CNTFRQ read 0), or a call count if the counter did not tick.

- [device] Nothing happened at 5 s: no drop before 7.56 s, and the 7.56 s drop came
  with an immediate re-enumeration (a reset, not a soft disconnect).
- **Reading 1: our own reset path, mis-timed** [inferred]. If the real time is r × the
  computed time, our PS_HOLD at 15 s lands at 7.56 s for r = 0.504, our watchdog at
  17.1 s for r = 0.442; that needs the soft disconnect to have done nothing (possible:
  whether our DWC3 writes take effect is exactly what's in doubt) and a timebase off by
  ~2×:

  | what is off | effect on the old timeline (5 s / 15 s) |
  |---|---|
  | nothing (counter 19.2 MHz, CNTFRQ 19.2 MHz or 0) | 5.0 / 15.0 s |
  | CNTFRQ = F (counter really 19.2 MHz) | 5·F/19.2 MHz / 15·F/19.2 MHz: F ≈ 9.7 MHz gives 2.5 / 7.56 s |
  | counter at 38.4 MHz, CNTFRQ 19.2 MHz | 2.5 / 7.5 s |
  | counter at 32 768 Hz, CNTFRQ 19.2 MHz | 49 min / 2.4 h |
  | CNTFRQ < 1000 (garbage) | division by zero → panic → our panic handler calls the same division → recursion until the stack overruns the image: a crash with interrupts masked, then whatever resets at 7.56 s |
  | counter stopped (call-count fallback, assumed 1 µs/call) | any; depends on loop speed |

  No known clock of this SoC runs the arch counter at 38.4 MHz [src: sdm630.dtsi, 19.2 MHz
  `xo_board`], so this reading needs a misprogrammed CNTFRQ or a stopped counter.
- **Reading 2: an external timer** [inferred]: something armed before the hand-over (a
  boot-ROM watchdog, petted during Sahara) bites 7.56 s after the last pet. 7.5 s is
  0x3C000 ticks of the 32 768 Hz sleep clock, numerology only. In E/D/F the immediate reset
  comes first.
- Probe U (if it shows 7.56 s, not 1.0 s) or probe G (mask and spin, nothing else)
  separates the two: a reset at 7.56 s with none of our code running is reading 2.
- [src] Probe R reads the APSS watchdog's enable bit at entry and shifts its timings by
  0.2 s if it was set: a set bit would make the APSS watchdog the prime suspect for
  reading 2.

### 8.4 Why B sent nothing: the endpoint command never reached the controller

- [src] **Bug, found in review:** in the old `ep_command()`, the writes of `DEPCMDPAR0..2`
  and `DEPCMD` and the poll of `DEPCMD` used the register offset without the DWC3 base
  (`depcmd(ep)` = `0xc83c` for physical endpoint 3, not `0x0a80c83c`). Only the read of the
  previous DEPCMD value had the base. So STARTTRANSFER (and the ENDTRANSFER retry) went to
  `0x0000c8xx`, whatever lies there, and the controller never saw a TRB.
- [inferred] That fits what the host saw exactly: the controller kept running with no
  transfer queued on bulk IN, so it NAKed the host's IN polls for the whole 7.56 s.
- [inferred] Those stray accesses did not raise an exception we caught (our handler would
  have soft-disconnected and reset at once), or they did and our handler failed to reset;
  the status image's fault codes will say which.
- Further fixes in the status image (`programmer-s`), each from Linux `drivers/usb/dwc3`:
  - GUSB2PHYCFG SUSPHY and ENBLSLPM cleared before the endpoint command
    (`gadget.c dwc3_send_gadget_ep_cmd()`, databook 6.3.2.5); `sdm630.dtsi` sets
    `snps,dis_u2_susphy_quirk` and `snps,dis_enblslpm_quirk` on this controller, so Linux
    keeps both off.
  - ENDTRANSFER with HIPRI_FORCERM and no CMDIOC is not polled, then 1 ms of delay
    (`__dwc3_stop_active_transfer()`).
  - The clock is a fixed 19.2 MHz (CNTFRQ only recorded): no division by a value the boot
    ROM left behind, no panic path that calls itself.
  - The status record grows to 192 bytes: adds GUSB2PHYCFG, GCTL, DEVTEN, the boot ROM's
    SP and LR at the hand-over, the APSS watchdog's enable/bite registers at entry, the
    DEPCMD value after a successful START, and the raw CNTFRQ.
- [unknown] Still open, and recorded in the record: which physical endpoint carries bulk
  IN (assumed the lowest enabled odd one ≥ 3: `0x81`); whether the controller can DMA
  from our image's RAM at `0x140168xx` (the boot ROM's event buffer address in the record
  shows where its own DMA memory is).

### 8.5 Measuring how far the code gets without USB

The status image ends by resetting at **T(code) = 0.6 s + 0.5 s × code** after the
jump (codes in `main.rs`, `enum Code`; table in TESTPLAN.md), through a chain so it does
not depend on one unproven mechanism: PS_HOLD at T; the APSS watchdog, armed at every
stage marker to bite at T + 0.15 s (also catches a hang right after a marker); at
T + 0.3 s the boot ROM's VBAR and DAIF are restored (the E path). The offset says which
mechanism worked. Everything ends before 7 s, so the 7.56 s reset cannot be mistaken for
a code. The timebase is the arch counter at 19.2 MHz, which probe U checks first.

## Sources

- Linux (mainline; checked against a 7.2/7.3-era tree): `arch/arm64/boot/dts/qcom/sdm630.dtsi`,
  `drivers/usb/dwc3/core.h`, `drivers/usb/dwc3/gadget.c`, `drivers/mmc/host/sdhci-msm.c`,
  `drivers/clk/qcom/gcc-sdm660.c`, `drivers/power/reset/msm-poweroff.c`,
  `drivers/input/misc/pm8941-pwrkey.c`, `drivers/watchdog/qcom-wdt.c`,
  `drivers/accel/qaic/sahara.c`, under
  <https://git.kernel.org/pub/scm/linux/kernel/git/torvalds/linux.git/tree/>.
- Aleph Security, "Exploiting Qualcomm EDL Programmers", parts
  [1](https://alephsecurity.com/2018/01/22/qualcomm-edl-1/) (PBL internals, software EDL)
  and [3](https://alephsecurity.com/2018/01/22/qualcomm-edl-3/) (programmer EL and
  registers per device); tooling <https://github.com/alephsecurity/firehorse>.
- coreboot: `util/qualcomm/mbn_tools.py`, `util/qualcomm/createxbl.py`, commit
  [101098c4](https://github.com/coreboot/coreboot/commit/101098c41a047184e3eceabca2c1baa11141f36e),
  `src/soc/qualcomm/sc7180/Makefile.mk`.
- qtestsign <https://github.com/msm8916-mainline/qtestsign> at
  `f3df53a5f0e37ccee076541f30f9d5b8340fc2d3` (README, `mbn/hashseg.py`, `mbn/cert.py`).
- bkerler/edl <https://github.com/bkerler/edl> (`edl.py`, `edlclient/Library/sahara.py`,
  `sahara_defs.py`); linux-msm/qdl <https://github.com/linux-msm/qdl>.
- Qualcomm ABL (CodeLinaro `clo/la/abl/tianocore/edk2`), `LinuxLoaderLib.c`,
  `FastbootCmds.c` (the `secure` variable).
