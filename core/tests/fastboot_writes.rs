// SPDX-License-Identifier: GPL-2.0-or-later
//! The fastboot writes (`flash_raw`, `oem`, `reboot-bootloader`) against the
//! simulated boot loader of tests/common/fbsim.rs: what lands in the
//! partition, every refusal before anything is sent, and every fault on the
//! way (a cut download, a FAIL on flash, a device that goes away, an early
//! OKAY).

mod common;
use common::fbsim::{FbSim, L, Wire};
use common::ramsim::image;
use glassine_installer::fastboot::*;
use glassine_installer::table::ReadAt;

const MIB: usize = 1 << 20;
const EQ: Framing = Framing::EqualLength { len: L };

fn block<F: Future>(f: F) -> F::Output {
    pollster::block_on(f)
}

fn parts() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        ("abl", image(MIB, 11)),
        ("misc", image(MIB, 12)),
        ("recovery", image(4 * MIB, 13)),
        ("boot", image(MIB, 14)),
        ("boot_a", image(MIB, 15)),
        ("boot_b", image(MIB, 16)),
        ("devinfo", image(4096, 17)),
    ]
}

/// The Note Air's description, as rules.
fn rules() -> WriteRules {
    WriteRules {
        framing: EQ,
        never_write: ["devinfo", "boot", "boot_a", "system", "system_a", "misc_never"].map(String::from).to_vec(),
        oem: vec!["reboot-recovery".into()],
    }
}

/// The opening and the warm-up, as inspect does them.
fn connect(sim: &mut FbSim, framing: Framing) -> Session<&mut FbSim> {
    block(async {
        let product = match opening(&mut *sim).await.unwrap() {
            Reply::Okay(p) => Some(p),
            r => panic!("opening: {r:?}"),
        };
        Session::start(sim, framing, product).await.unwrap()
    })
}

fn flash(s: &mut Session<&mut FbSim>, r: &WriteRules, part: &str, img: &[u8]) -> Result<(), WriteError> {
    let mut src: &[u8] = img;
    block(s.flash_raw(r, part, &mut src, img.len() as u64))
}

/// A source that remembers the largest read, and fails at an offset.
struct Src<'a> {
    data: &'a [u8],
    largest: usize,
    reads: usize,
    fail_at: Option<u64>,
}

impl ReadAt for Src<'_> {
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<(), String> {
        if self.fail_at.is_some_and(|f| offset >= f) {
            return Err("the file went away".into());
        }
        self.largest = self.largest.max(buf.len());
        self.reads += 1;
        let mut d = self.data;
        d.read_at(offset, buf)
    }
}

/// The partitions are what they were.
fn untouched(sim: &FbSim) {
    for (name, data) in parts() {
        assert_eq!(sim.parts[name], data, "{name} changed");
    }
    assert!(!sim.erased_whole_disk);
}

/// Refused with nothing sent at all, and the session goes on.
fn refused_unsent(sim: &mut FbSim, r: &WriteRules, part: &str, img: &[u8]) -> FbError {
    let mut s = connect(sim, EQ);
    let before = s.transport().log.len();
    let e = flash(&mut s, r, part, img).unwrap_err();
    assert_eq!(s.transport().log.len(), before, "{part}: something was sent");
    assert!(s.ended().is_none());
    let product = s.product().map(String::from);
    assert_eq!(block(s.getvar("product")).unwrap(), product);
    drop(s);
    untouched(sim);
    match e {
        WriteError::Refused(e) => e,
        e => panic!("{part}: {e:?}"),
    }
}

// ---- what lands -------------------------------------------------------------

#[test]
fn stock_flash_of_misc_lands_with_equal_length_framing() {
    let mut sim = FbSim::stock_noteair(&parts());
    let img = image(16 * L, 99);
    let mut s = connect(&mut sim, EQ);
    flash(&mut s, &rules(), "misc", &img).unwrap();
    drop(s);
    assert_eq!(&sim.parts["misc"][..img.len()], &img[..]);
    // the bytes after the download stay
    assert_eq!(&sim.parts["misc"][img.len()..], &image(MIB, 12)[img.len()..]);
    assert_eq!(
        sim.commands,
        [
            "getvar:product",
            "xxxxxx",
            "getvar:product",
            "getvar:partition-size:misc",
            "getvar:max-download-size",
            "download:000ffff0",
            "getvar:product",
            "flash:misc",
        ]
    );
    // after the opening, every transfer is exactly L bytes
    assert!(sim.log[1..].iter().all(|t| t.len() == L));
}

#[test]
fn padding_lands_in_the_partition_and_the_rest_stays() {
    // 1000 bytes: not a whole 512-byte block; padded to one transfer.
    let mut sim = FbSim::stock_noteair(&parts());
    let img = image(1000, 7);
    let mut s = connect(&mut sim, EQ);
    flash(&mut s, &rules(), "misc", &img).unwrap();
    drop(s);
    let m = &sim.parts["misc"];
    assert_eq!(&m[..1000], &img[..]);
    assert!(m[1000..L].iter().all(|&b| b == 0));
    assert_eq!(&m[L..], &image(MIB, 12)[L..]);
}

#[test]
fn glassine_boot_loader_takes_32_mib() {
    let mut sim = FbSim::glassine_noteair(&[("cache", image(40 * MIB, 3))]);
    let img = image(32 * MIB, 4);
    let mut s = connect(&mut sim, EQ);
    let mut src = Src {
        data: &img,
        largest: 0,
        reads: 0,
        fail_at: None,
    };
    block(s.flash_raw(&rules(), "cache", &mut src, img.len() as u64)).unwrap();
    drop(s);
    assert_eq!(&sim.parts["cache"][..img.len()], &img[..]);
    // streamed: never more than one transfer read at a time
    assert!(src.largest <= L, "read {} bytes at once", src.largest);
    assert!(src.reads >= img.len() / L);
}

#[test]
fn plain_framing_writes_on_a_standard_device() {
    let mut sim = FbSim::new(Wire::Standard, "generic", 8 * MIB as u64, &[("boot_b", image(4 * MIB, 1))]);
    let r = WriteRules {
        framing: Framing::Plain,
        never_write: vec![],
        oem: vec![],
    };
    let img = image(3 * MIB + 5, 2);
    let mut s = connect(&mut sim, Framing::Plain);
    flash(&mut s, &r, "boot_b", &img).unwrap();
    drop(s);
    assert_eq!(&sim.parts["boot_b"][..img.len()], &img[..]);
    assert!(sim.commands.contains(&format!("download:{:08x}", img.len())));
}

#[test]
fn abl_takes_16_transfers_and_not_17() {
    let mut sim = FbSim::stock_noteair(&parts());
    let mut s = connect(&mut sim, EQ);
    flash(&mut s, &rules(), "abl", &image(16 * L, 5)).unwrap();
    // one byte more pads to 17 transfers: 1114095 > 1 MiB
    let e = flash(&mut s, &rules(), "abl", &image(16 * L + 1, 6)).unwrap_err();
    assert!(matches!(e, WriteError::Refused(FbError::RefusedWrite(ref w)) if w.contains("1114095")), "{e:?}");
    assert!(s.ended().is_none());
    drop(s);
    assert_eq!(&sim.parts["abl"][..16 * L], &image(16 * L, 5)[..]);
    assert_eq!(sim.commands.iter().filter(|c| c.starts_with("download:")).count(), 1);
}

// ---- refusals ---------------------------------------------------------------

#[test]
fn the_partition_table_is_never_flashed() {
    for name in [
        "partition",
        "PARTITION",
        "Partition",
        "partition:0",
        "partition:1",
        "partitionfoo",
        "partition_a",
        "partition\0x",
    ] {
        let mut sim = FbSim::stock_noteair(&parts());
        let e = refused_unsent(&mut sim, &rules(), name, &image(L, 1));
        assert!(matches!(e, FbError::RefusedWrite(_)), "{name}: {e:?}");
        assert!(!sim.sent("partition"), "{name}");
    }
    // even with rules that list nothing
    let open = WriteRules {
        framing: EQ,
        never_write: vec![],
        oem: vec![],
    };
    let mut sim = FbSim::stock_noteair(&parts());
    refused_unsent(&mut sim, &open, "partition", &image(L, 1));
}

#[test]
fn the_simulation_erases_the_whole_disk_on_flash_partition() {
    // What the client is kept from: the model does what the boot loaders do.
    let mut sim = FbSim::new(Wire::Standard, "sdm660", 8 * MIB as u64, &parts());
    block(async {
        sim.send(b"download:00000200").await.unwrap();
        sim.recv().await.unwrap();
        sim.send(&[0u8; 512]).await.unwrap();
        sim.recv().await.unwrap();
        sim.send(b"flash:partition:0").await.unwrap();
        assert_eq!(sim.recv().await.unwrap(), b"OKAY");
    });
    assert!(sim.erased_whole_disk);
    assert!(sim.parts.values().all(|p| p.iter().all(|&b| b == 0)));
}

#[test]
fn never_write_names_are_refused() {
    for name in ["devinfo", "DevInfo", "boot", "boot_a", "system", "SYSTEM_A"] {
        let mut sim = FbSim::stock_noteair(&parts());
        refused_unsent(&mut sim, &rules(), name, &image(L, 1));
    }
    // `system` is refused for system_a too: an A/B boot loader adds the suffix
    let r = WriteRules {
        never_write: vec!["vendor_a".into()],
        ..rules()
    };
    let mut sim = FbSim::stock_noteair(&parts());
    let e = refused_unsent(&mut sim, &r, "vendor", &image(L, 1));
    assert!(e.to_string().contains("vendor_a"), "{e}");
    // boot_b is not on the list
    let mut sim = FbSim::stock_noteair(&parts());
    let mut s = connect(&mut sim, EQ);
    flash(&mut s, &rules(), "boot_b", &image(L, 2)).unwrap();
}

#[test]
fn names_that_are_not_one_partition_are_refused() {
    let long = "a".repeat(COMMAND_MAX - "flash:".len() + 1);
    for name in ["", "a b", "x:y", "abl\0", "abl\n", "../abl", "abl;reboot", long.as_str()] {
        let mut sim = FbSim::stock_noteair(&parts());
        refused_unsent(&mut sim, &rules(), name, &image(L, 1));
    }
}

#[test]
fn a_plain_session_writes_nothing_where_equal_length_is_needed() {
    let mut sim = FbSim::new(Wire::Standard, "sdm660", 8 * MIB as u64, &parts());
    let mut s = connect(&mut sim, Framing::Plain);
    let before = s.transport().log.len();
    let e = flash(&mut s, &rules(), "misc", &image(L, 1)).unwrap_err();
    assert!(
        matches!(e, WriteError::Refused(FbError::RefusedWrite(ref w)) if w.contains("equal-length")),
        "{e:?}"
    );
    let e = block(s.oem(&rules(), "reboot-recovery")).unwrap_err();
    assert!(matches!(e, FbError::RefusedWrite(_)), "{e:?}");
    assert_eq!(s.transport().log.len(), before);
    drop(s);
    untouched(&sim);
}

#[test]
fn an_equal_length_session_writes_nothing_where_plain_is_described() {
    let mut sim = FbSim::stock_noteair(&parts());
    let r = WriteRules {
        framing: Framing::Plain,
        ..rules()
    };
    refused_unsent(&mut sim, &r, "misc", &image(L, 1));
}

#[test]
fn empty_and_not_raw_images_are_refused() {
    let mut sim = FbSim::stock_noteair(&parts());
    refused_unsent(&mut sim, &rules(), "misc", &[]);
    for magic in [0xed26_ff3au32.to_le_bytes(), 0xce1a_d63cu32.to_le_bytes(), *b"UBI#"] {
        let mut img = image(L, 1);
        img[..4].copy_from_slice(&magic);
        let mut sim = FbSim::stock_noteair(&parts());
        let e = refused_unsent(&mut sim, &rules(), "misc", &img);
        assert!(e.to_string().contains("starts like"), "{e}");
    }
}

#[test]
fn the_simulation_answers_a_sparse_image_early_where_the_client_never_sends_one() {
    // abl answers OKAY before it writes a sparse image into a partition
    // larger than max-download-size; the client refuses sparse images, so it
    // never meets that OKAY. The model:
    let mut sim = FbSim::new(Wire::Standard, "sdm660", 4096, &[("big", vec![0; 8192])]);
    block(async {
        sim.send(b"download:00000200").await.unwrap();
        sim.recv().await.unwrap();
        let mut d = vec![1u8; 512];
        d[..4].copy_from_slice(&0xed26_ff3au32.to_le_bytes());
        sim.send(&d).await.unwrap();
        sim.recv().await.unwrap();
        sim.send(b"flash:big").await.unwrap();
        sim.recv().await.unwrap();
    });
    assert!(sim.early_okay);
}

#[test]
fn sizes_the_boot_loader_does_not_take_are_refused_after_reads_only() {
    // more than max-download-size
    let mut sim = FbSim::stock_noteair(&parts());
    sim.max_download = 4 * L as u64;
    let mut s = connect(&mut sim, EQ);
    let e = flash(&mut s, &rules(), "recovery", &image(4 * L + 1, 1)).unwrap_err();
    assert!(
        matches!(e, WriteError::Refused(FbError::RefusedWrite(ref w)) if w.contains("more than the boot loader takes")),
        "{e:?}"
    );
    assert!(s.ended().is_none());
    drop(s);
    assert!(sim.commands.iter().all(|c| c.starts_with("getvar:") || c == "xxxxxx"), "{:?}", sim.commands);
    untouched(&sim);

    // a partition the boot loader doesn't know
    let mut sim = FbSim::stock_noteair(&parts());
    let mut s = connect(&mut sim, EQ);
    let e = flash(&mut s, &rules(), "boot_c", &image(L, 1)).unwrap_err();
    assert!(
        matches!(e, WriteError::Refused(FbError::RefusedWrite(ref w)) if w.contains("partition-size")),
        "{e:?}"
    );
    drop(s);
    assert!(sim.commands.iter().all(|c| c.starts_with("getvar:") || c == "xxxxxx"));

    // a boot loader that doesn't say how much it takes
    let mut sim = FbSim::stock_noteair(&parts());
    sim.hidden.push("max-download-size".into());
    let mut s = connect(&mut sim, EQ);
    let e = flash(&mut s, &rules(), "misc", &image(L, 1)).unwrap_err();
    assert!(
        matches!(e, WriteError::Refused(FbError::RefusedWrite(ref w)) if w.contains("max-download-size")),
        "{e:?}"
    );
    drop(s);
    untouched(&sim);
}

#[test]
fn numbers_in_both_forms() {
    assert_eq!(parse_number("0x100000"), Some(1 << 20));
    assert_eq!(parse_number("595230720"), Some(595230720));
    assert_eq!(parse_number("lots"), None);
    assert_eq!(download_size(16 * L as u64, EQ), 16 * L as u64);
    assert_eq!(download_size(16 * L as u64 + 1, EQ), 17 * L as u64);
}

// ---- faults -----------------------------------------------------------------

/// A fault before `flash:`: the partition is untouched, `flash:` never went
/// out, and the session has ended.
fn before_flash(sim: &mut FbSim, img: &[u8]) -> FbError {
    let mut s = connect(sim, EQ);
    let e = flash(&mut s, &rules(), "misc", img).unwrap_err();
    assert!(!e.partition_may_have_changed());
    assert!(s.ended().is_some(), "{e:?}");
    // nothing more goes out on an ended session
    let n = s.transport().log.len();
    assert!(matches!(block(s.getvar("product")), Err(FbError::Ended(_))));
    assert!(matches!(flash(&mut s, &rules(), "misc", img), Err(WriteError::Refused(FbError::Ended(_)))));
    assert_eq!(s.transport().log.len(), n);
    drop(s);
    assert!(!sim.commands.iter().any(|c| c.starts_with("flash:")), "{:?}", sim.commands);
    untouched(sim);
    match e {
        WriteError::BeforeFlash(e) => e,
        e => panic!("{e:?}"),
    }
}

#[test]
fn a_cut_mid_download_never_reaches_the_partition() {
    let mut sim = FbSim::stock_noteair(&parts());
    sim.faults.cut_at_chunk = Some(7);
    let e = before_flash(&mut sim, &image(16 * L, 1));
    assert!(matches!(e, FbError::Usb(_)), "{e:?}");
}

#[test]
fn a_device_that_vanishes_during_the_download() {
    // the opening, dummy, product, two size reads, download: then gone
    let mut sim = FbSim::stock_noteair(&parts());
    sim.faults.vanish_after = Some(6);
    let e = before_flash(&mut sim, &image(16 * L, 1));
    assert!(matches!(e, FbError::Usb(_)), "{e:?}");
}

#[test]
fn a_device_that_vanishes_after_the_download_gets_no_flash() {
    // ... 16 data transfers, then the product check fails
    let mut sim = FbSim::stock_noteair(&parts());
    sim.faults.vanish_after = Some(6 + 16);
    before_flash(&mut sim, &image(16 * L, 1));
}

#[test]
fn okay_where_data_is_due() {
    let mut sim = FbSim::stock_noteair(&parts());
    sim.faults.okay_for_data = true;
    let e = before_flash(&mut sim, &image(16 * L, 1));
    assert!(matches!(e, FbError::Malformed(_)), "{e:?}");
}

#[test]
fn okay_before_all_the_data_is_caught_by_the_product_check() {
    // The rest of the data is read as commands; the product check after the
    // download gets their FAILs, so flash: is never sent.
    let mut sim = FbSim::stock_noteair(&parts());
    sim.faults.okay_after_chunks = Some(5);
    let e = before_flash(&mut sim, &image(16 * L, 1));
    assert!(matches!(e, FbError::NotFresh(_)), "{e:?}");
}

#[test]
fn another_download_size_than_asked() {
    let mut sim = FbSim::stock_noteair(&parts());
    sim.faults.data_size = Some(L as u32);
    let e = before_flash(&mut sim, &image(16 * L, 1));
    assert!(matches!(e, FbError::Fail(ref w) if w.contains("takes")), "{e:?}");
}

#[test]
fn an_image_that_cannot_be_read_mid_stream() {
    let mut sim = FbSim::stock_noteair(&parts());
    let img = image(16 * L, 1);
    let mut s = connect(&mut sim, EQ);
    let mut src = Src {
        data: &img,
        largest: 0,
        reads: 0,
        fail_at: Some(8 * L as u64),
    };
    let e = block(s.flash_raw(&rules(), "misc", &mut src, img.len() as u64)).unwrap_err();
    assert!(matches!(e, WriteError::BeforeFlash(_)), "{e:?}");
    assert!(s.ended().is_some());
    drop(s);
    assert!(!sim.commands.iter().any(|c| c.starts_with("flash:")));
    untouched(&sim);
}

#[test]
fn a_short_last_transfer_overruns_on_the_note_air() {
    // Why the client pads: the Note Air counts a transfer as long as the one
    // before it, so a short last one brings bytes nobody sent.
    let mut sim = FbSim::stock_noteair(&parts());
    block(async {
        sim.send(b"getvar:product").await.unwrap();
        sim.recv().await.unwrap();
        sim.send(&vec![b'x'; L]).await.unwrap();
        sim.recv().await.unwrap();
        let mut c = vec![0u8; L];
        c[..17].copy_from_slice(b"download:00010000");
        sim.send(&c).await.unwrap();
        assert_eq!(sim.recv().await.unwrap(), b"DATA00010000");
        sim.send(&vec![1u8; L]).await.unwrap();
        sim.send(&[1u8]).await.unwrap();
        assert_eq!(sim.recv().await.unwrap(), b"FAILdata overrun");
    });
}

#[test]
fn fail_on_flash_says_the_partition_may_have_changed() {
    let mut sim = FbSim::stock_noteair(&parts());
    sim.faults.fail_flash = true;
    let mut s = connect(&mut sim, EQ);
    let e = flash(&mut s, &rules(), "misc", &image(16 * L, 1)).unwrap_err();
    assert!(e.partition_may_have_changed());
    assert!(matches!(e, WriteError::Flash(FbError::Fail(_))), "{e:?}");
    drop(s);
    assert_ne!(sim.parts["misc"], image(MIB, 12));
}

#[test]
fn no_answer_to_flash_says_the_partition_may_have_changed() {
    let mut sim = FbSim::stock_noteair(&parts());
    // ... 16 data transfers, the product check, then flash: is the last one sent
    sim.faults.vanish_after = Some(6 + 16 + 1);
    let mut s = connect(&mut sim, EQ);
    let e = flash(&mut s, &rules(), "misc", &image(16 * L, 1)).unwrap_err();
    assert!(matches!(e, WriteError::Flash(_)), "{e:?}");
    assert!(e.partition_may_have_changed());
    assert!(s.ended().is_some());
}

// ---- oem and reboot-bootloader ----------------------------------------------

#[test]
fn oem_only_for_allowed_verbs() {
    for verb in ["reboot-edl", "", "reboot-recovery now", "reboot-recovery\0", "Reboot-Recovery", "device-info"] {
        let mut sim = FbSim::glassine_noteair(&parts());
        let mut s = connect(&mut sim, EQ);
        let n = s.transport().log.len();
        let e = block(s.oem(&rules(), verb)).unwrap_err();
        assert!(matches!(e, FbError::RefusedWrite(_)), "{verb:?}: {e:?}");
        assert_eq!(s.transport().log.len(), n);
    }
    let mut sim = FbSim::glassine_noteair(&parts());
    let mut s = connect(&mut sim, EQ);
    block(s.oem(&rules(), "reboot-recovery")).unwrap();
    drop(s);
    assert_eq!(sim.commands.last().unwrap(), "oem reboot-recovery");
    assert_eq!(sim.rebooted, ["reboot-recovery"]);
}

#[test]
fn oem_that_the_boot_loader_does_not_know_fails() {
    // the stock boot loader has no oem reboot-recovery
    let mut sim = FbSim::stock_noteair(&parts());
    let mut s = connect(&mut sim, EQ);
    let e = block(s.oem(&rules(), "reboot-recovery")).unwrap_err();
    assert!(matches!(e, FbError::Fail(_)), "{e:?}");
}

#[test]
fn reboot_bootloader_restarts_into_fastboot() {
    let mut sim = FbSim::stock_noteair(&parts());
    let mut s = connect(&mut sim, EQ);
    block(s.reboot_bootloader()).unwrap();
    drop(s);
    assert_eq!(sim.commands.last().unwrap(), "reboot-bootloader");
    assert_eq!(sim.rebooted, ["bootloader"]);
    // a fresh boot takes a new session
    sim.boot_again();
    let mut s = connect(&mut sim, EQ);
    flash(&mut s, &rules(), "misc", &image(L, 3)).unwrap();
}
