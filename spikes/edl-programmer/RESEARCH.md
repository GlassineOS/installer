# A GlassineOS programmer for the SDM636 boot ROM: research

Spike for [installer#18](https://github.com/GlassineOS/installer/issues/18) step 1 and
[spec 004](../../specs/004-emergency-mode/spec.md) (branch `emergency-mode`). Host-only work:
nothing here was run on a device.

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

## 1. Will the PBL run an image we sign ourselves?

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
