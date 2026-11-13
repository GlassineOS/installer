# Device test plan: first upload of the GlassineOS programmer (issue #18 step 3)

**Gated:** only with the owner's go, the EDL cable at hand, and after step 2 (Sahara command
mode, read-only) has shown the unit's MSM_HW_ID and OEM_PK_HASH. Nothing in this test can
write storage: the image has no eMMC code (it only touches the USB controller, PS_HOLD,
the APSS watchdog and its own RAM).

## Before

1. Build: `./build.sh`; compare `out/SHA256SUMS` with the hashes in the PR.
2. Host: `qcserial` blacklisted, nothing else holding the 9008 device, the host client
   started before the device is plugged in or rebooted to 9008 (the HELLO window is short).
3. Device: Android running, USB debugging on, battery > 50 %. Note the time.
4. `adb reboot edl`; wait for `05c6:9008`.

## Run (one image per 9008 session)

Order: `programmer-a-v5.mbn`; if refused or no sign of life, `programmer-b-v5.mbn`; if
both are refused with an authentication status, `programmer-a-v3.mbn`.

Host steps (Sahara client in the installer core, or bkerler's edl `--loader` with only the
upload; no Firehose command is sent):

1. HELLO from the device → HELLO_RESP, mode IMAGE_TX_PENDING.
2. Serve every READ_DATA / READ_DATA64 from the image. Log each request (offset, length):
   the order shows what the PBL reads (ELF header, program headers, hash segment, then
   each segment) and whether it asks for the XBL_SEC segment of variant B.
3. **END_OF_IMAGE**: status 0 = accepted. Anything else is a refusal; record the status
   (0x21/0x22/0x23/0x25 = authentication, 0x12 = destination address, 0x14 = ELF header).
   A refusal is harmless: the PBL stays in Sahara; send RESET or long-press power.
4. DONE → **DONE_RESP** with image_tx_status 1 (complete). From here the PBL has jumped.
   Mark this moment T0.
5. Within ~4 s of T0: bulk-IN read, 512-byte buffer, timeout 4 s.

## Success, step by step

| Observation | Means |
|---|---|
| END_OF_IMAGE status 0 | the PBL accepted a test-signed image of ours (the secure-boot inference holds) |
| DONE_RESP complete | the PBL jumped to it |
| 128 bytes starting `GLSNPRG0` on bulk IN | our code ran and drove the PBL's USB device |
| `05c6:9008` disappears at ≈ T0 + 5 s | our code ran (soft disconnect), even if no record came |
| the device comes back at ≈ T0 + 15 s (as 9008, Android, charging screen) or switches off | our reset path works (which of these = the PMIC's PS_HOLD setting) |

Status record (little-endian u32 words):

| Word | Field |
|---|---|
| 0–1 | `GLSN` `PRG0` |
| 2, 3 | record version 1, length 128 |
| 4 | exception level at entry |
| 5 | flags: bit0 counter ticks, bit1 bulk-IN found, bit2 START timed out, bit3 END+START retry, bit4 entered through the XBL_SEC trampoline |
| 6–7, 8–9 | MIDR_EL1, MPIDR_EL1 |
| 10–11 | SCTLR_ELx at entry (MMU/caches) |
| 12–13 | VBAR_ELx the PBL had (its vector table) |
| 14–15 | DAIF as the PBL left it (read before masking) |
| 16–17 | counter frequency used |
| 18–22 | DWC3 GSNPSID, DCFG, DCTL, DSTS, DALEPENA |
| 23 | physical bulk-IN endpoint used |
| 24 | its DEPCMD before our command (the PBL's last command, resource index) |
| 25, 26 | attempt number; DEPCMD of a failed START |
| 27–30 | event buffer 0: address lo/hi, size, count (where the PBL keeps it) |
| 31 | address of `glassine_main` (sanity: `0x140161xx`) |

Everything in it feeds the design of step 4 (EL and MMU state, where the PBL's USB
buffers are).

## Failure and escape

- **Refusal in Sahara** (step 3): harmless. Sahara RESET, or long-press power.
- **No record, no disconnect, 9008 stays listed but silent:** the image crashed or hangs
  before touching USB (e.g. entered at an unexpected EL, or an XBL_SEC expectation).
  Long-press power (~10 s) resets out of 9008 [lab]. Then try variant B.
- **Device vanished and never comes back:** either PS_HOLD switched it off (press power
  to start) or it hangs with USB off. Long-press power. If the power key does nothing: the
  EDL cable / battery-drain route the lab documented (no storage was touched, so Android
  boots normally afterwards).
- **900E appears:** the watchdog bite path landed in crash-dump mode. Long-press power.
- After any outcome: Android must boot unchanged (it can: the image has no write path).
  Check with `adb shell getprop sys.boot_completed`.

## Not part of this test

eMMC access, any Firehose command, any write. Re-enumeration with our own VID/PID.
