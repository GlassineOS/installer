// SPDX-License-Identifier: GPL-2.0-or-later
//! The check of a file the person supplies for a mode (spec 004 FR-004,
//! FR-005; spec 002 FR-018b), before anything is sent: its checksum against
//! the description's known ones, then whether the route of the mode that
//! needs it recognises it as made for the values its `matches` names, and,
//! once the device is in that mode, whether the device reports the same.
//!
//! The check only reads the bytes it is given. It never sends, stores or
//! uploads them, and it never builds a program (FR-005). The routes'
//! recognition is behind [`recognise`]; the first is Qualcomm's emergency
//! download mode ([`qualcomm`]).

use crate::format1::{Description, SuppliedFile};
use crate::vocab;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

/// The largest file the check reads: a programmer is well under a megabyte;
/// anything this big is something else.
pub const MAX_BYTES: usize = 32 << 20;

/// What the device reports in the mode, by the route's match keys (for the
/// emergency-download route `chip` and `pk_hash`, as
/// [`crate::sahara::EmergencyFacts`] gives them).
pub type DeviceValues = BTreeMap<String, String>;

/// The boot ROM's facts as the emergency-download route checks them.
pub fn emergency_values(f: &crate::sahara::EmergencyFacts) -> DeviceValues {
    BTreeMap::from([("chip".to_string(), format!("{:08x}", f.hw().msm_id)), ("pk_hash".to_string(), f.pk_hash_hex())])
}

/// The check's answer (004 Scenario 3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "verdict", rename_all = "kebab-case")]
pub enum Verdict {
    /// The same, byte for byte, as a file known to work on this device.
    Known {
        /// Whether that was in a recorded run (FR-023a), not only in development.
        verified: bool,
    },
    /// Made for this chip and key, but not a known file: sent only after the
    /// person confirms a warning.
    FitsUnknown { values: BTreeMap<String, String> },
    /// Made for another chip or key, or the device reports other values than
    /// the description names: never sent. `made_for` is what the file is
    /// made for (empty when that couldn't be read; for a known file, the
    /// description's values), `want` the description's values, `reported`
    /// what the device reported (when it was asked), `differs` the keys that
    /// differ.
    WrongChipOrKey {
        made_for: BTreeMap<String, String>,
        want: BTreeMap<String, String>,
        reported: Option<BTreeMap<String, String>>,
        differs: Vec<String>,
    },
    /// Not something the route recognises as a program for it: never sent.
    NotAProgrammer { why: String },
}

impl Verdict {
    /// Whether the file may be sent: without asking (`Some(false)`), after
    /// the person confirms (`Some(true)`), or never (`None`).
    pub fn sendable(&self) -> Option<bool> {
        match self {
            Verdict::Known { .. } => Some(false),
            Verdict::FitsUnknown { .. } => Some(true),
            _ => None,
        }
    }
}

/// The check of one file, as a record (`supplied_check_format` 1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Check {
    pub supplied_check_format: u32,
    pub device: String,
    pub file: String,
    pub route: String,
    pub size: u64,
    pub sha256: String,
    #[serde(flatten)]
    pub verdict: Verdict,
}

/// Checks `bytes` as the supplied file `file_id` of the description `d`.
/// `device` is what the device reports in the mode, once it is there. An
/// error is a question the description can't answer (no such file, no mode
/// that needs it), never a verdict on the file.
pub fn check(d: &Description, file_id: &str, bytes: &[u8], device: Option<&DeviceValues>) -> Result<Check, String> {
    let f = d.supplied_file(file_id).ok_or_else(|| format!("{} has no supplied file {file_id:?}", d.id))?;
    let route = d
        .modes
        .iter()
        .find(|m| m.needs.as_deref() == Some(file_id))
        .map(|m| m.kind.clone())
        .ok_or_else(|| format!("no mode of {} needs {file_id:?}", d.id))?;
    let sha256 = hex(&Sha256::digest(bytes));
    Ok(Check {
        supplied_check_format: 1,
        device: d.id.clone(),
        file: f.id.clone(),
        route: route.clone(),
        size: bytes.len() as u64,
        verdict: verdict(&route, f, bytes, &sha256, device),
        sha256,
    })
}

fn verdict(route: &str, f: &SuppliedFile, bytes: &[u8], sha256: &str, device: Option<&DeviceValues>) -> Verdict {
    let keys: Vec<&str> = vocab::supplied_matches(route).iter().map(|m| m.key).collect();
    let want: BTreeMap<String, String> = keys
        .iter()
        .filter_map(|k| f.matches.get(*k).map(|v| (k.to_string(), v.to_ascii_lowercase())))
        .collect();
    let known = f.known.iter().find(|k| k.sha256.eq_ignore_ascii_case(sha256));
    let found = if known.is_some() { Ok(want.clone()) } else { recognise(route, bytes, &want) };
    // The device's own report decides first: a chip or key the description
    // doesn't name sends nothing, whatever the file (004 edge cases).
    if let Some(dev) = device {
        let dev: BTreeMap<String, String> = keys
            .iter()
            .map(|k| (k.to_string(), dev.get(*k).map(|v| v.to_ascii_lowercase()).unwrap_or_default()))
            .collect();
        let differs: Vec<String> = keys.iter().filter(|k| dev.get(**k) != want.get(**k)).map(|k| k.to_string()).collect();
        if !differs.is_empty() {
            return Verdict::WrongChipOrKey {
                made_for: found.unwrap_or_default(),
                want,
                reported: Some(dev),
                differs,
            };
        }
    }
    if let Some(k) = known {
        return Verdict::Known {
            verified: k.verified.is_some(),
        };
    }
    let values = match found {
        Ok(v) => v,
        Err(why) => return Verdict::NotAProgrammer { why },
    };
    let differs: Vec<String> = keys.iter().filter(|k| values.get(**k) != want.get(**k)).map(|k| k.to_string()).collect();
    if differs.is_empty() && !keys.is_empty() {
        Verdict::FitsUnknown { values }
    } else {
        Verdict::WrongChipOrKey {
            made_for: values,
            want,
            reported: device.cloned(),
            differs,
        }
    }
}

/// What the route reads from a file: its value for each of the route's
/// match keys, or why it isn't a program for the route. `want` tells the
/// form a value is compared in (the key hash's length picks its algorithm).
pub fn recognise(route: &str, bytes: &[u8], want: &BTreeMap<String, String>) -> Result<BTreeMap<String, String>, String> {
    if bytes.len() > MAX_BYTES {
        return Err(format!("{} bytes is far too big for a programmer", bytes.len()));
    }
    match route {
        "emergency-download" => {
            let wide = want.get("pk_hash").is_some_and(|h| h.len() == 96);
            let s = qualcomm::read(bytes)?;
            Ok(BTreeMap::from([
                ("chip".to_string(), s.chip),
                ("pk_hash".to_string(), if wide { s.root_sha384 } else { s.root_sha256 }),
            ]))
        }
        other => Err(format!("the {other} route recognises no supplied program")),
    }
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Qualcomm's signed images (MBN in an ELF): the hash segment's certificate
/// chain says which chip the image is for (the attestation certificate's
/// `HW_ID`) and which key signed it (the root certificate, whose hash the
/// boot ROM reports as the OEM PK hash). Read from the public descriptions
/// of the format (Qualcomm's secure boot overview, qtestsign, coreboot's
/// mbn_tools); nothing here is verified cryptographically: it tells what a
/// file claims, so a file for another device is refused before it is sent.
pub mod qualcomm {
    use super::hex;
    use sha2::{Digest, Sha256, Sha384};

    /// The segment-type bits of a program header's flags, and the hash
    /// segment's type.
    pub const SEGMENT_TYPE_MASK: u32 = 0x0700_0000;
    pub const SEGMENT_TYPE_HASH: u32 = 0x0200_0000;

    /// What a signed image says about itself.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Signed {
        /// The hash segment's header version (3 or 5).
        pub version: u32,
        /// `HW_ID` from the attestation certificate, 16 lower-case hex digits.
        pub hw_id: String,
        /// Its first 8 digits, the msm id; or `soc-hw-version:XXXX` when the
        /// certificate says the HW_ID names the SoC's hardware family.
        pub chip: String,
        pub root_sha256: String,
        pub root_sha384: String,
        /// How many certificates the chain has.
        pub certificates: usize,
    }

    fn u16le(b: &[u8], at: usize) -> Result<u16, String> {
        b.get(at..at.checked_add(2).ok_or("offset overflows")?)
            .map(|x| u16::from_le_bytes([x[0], x[1]]))
            .ok_or_else(|| "cut short".to_string())
    }

    fn u32le(b: &[u8], at: usize) -> Result<u32, String> {
        b.get(at..at.checked_add(4).ok_or("offset overflows")?)
            .map(|x| u32::from_le_bytes([x[0], x[1], x[2], x[3]]))
            .ok_or_else(|| "cut short".to_string())
    }

    fn u64le(b: &[u8], at: usize) -> Result<u64, String> {
        b.get(at..at.checked_add(8).ok_or("offset overflows")?)
            .map(|x| u64::from_le_bytes(x.try_into().unwrap()))
            .ok_or_else(|| "cut short".to_string())
    }

    fn size(v: u64) -> Result<usize, String> {
        usize::try_from(v).map_err(|_| "a size beyond this computer's memory".to_string())
    }

    /// One program header: flags, offset and size in the file.
    struct Phdr {
        flags: u32,
        offset: usize,
        filesz: usize,
    }

    fn program_headers(b: &[u8]) -> Result<Vec<Phdr>, String> {
        if b.len() < 16 || &b[..4] != b"\x7fELF" {
            return Err("not an ELF file".into());
        }
        if b[5] != 1 {
            return Err("not a little-endian ELF file".into());
        }
        let (phoff, entsize, num, wide) = match b[4] {
            1 => (u32le(b, 0x1c)? as u64, u16le(b, 0x2a)?, u16le(b, 0x2c)?, false),
            2 => (u64le(b, 0x20)?, u16le(b, 0x36)?, u16le(b, 0x38)?, true),
            _ => return Err("an ELF class other than 32 or 64 bits".into()),
        };
        let want = if wide { 56 } else { 32 };
        if entsize as usize != want {
            return Err(format!("program headers of {entsize} bytes, not {want}"));
        }
        let phoff = size(phoff)?;
        let mut out = Vec::new();
        for i in 0..num as usize {
            let at = i.checked_mul(want).and_then(|x| x.checked_add(phoff)).ok_or("offset overflows")?;
            // The whole header is in the file, so the reads below stay in it.
            if at.checked_add(want).is_none_or(|end| end > b.len()) {
                return Err("cut short: the program headers end beyond the file".into());
            }
            let p = if wide {
                Phdr {
                    flags: u32le(b, at + 4)?,
                    offset: size(u64le(b, at + 8)?)?,
                    filesz: size(u64le(b, at + 32)?)?,
                }
            } else {
                Phdr {
                    flags: u32le(b, at + 24)?,
                    offset: u32le(b, at + 4)? as usize,
                    filesz: u32le(b, at + 16)? as usize,
                }
            };
            // Every segment is in the file: a file cut short is never read as whole.
            match p.offset.checked_add(p.filesz) {
                Some(end) if end <= b.len() => {}
                _ => return Err("cut short: a segment ends beyond the file".into()),
            }
            out.push(p);
        }
        Ok(out)
    }

    /// The certificate chains of the hash segment: the OEM's, then
    /// Qualcomm's (either may be empty).
    fn chains(seg: &[u8]) -> Result<(u32, &[u8], &[u8]), String> {
        let version = u32le(seg, 4)?;
        let w = |i: usize| u32le(seg, i * 4).map(|x| x as usize);
        let (hashes, sig, chain) = (w(5)?, w(7)?, w(9)?);
        let (sig_q, chain_q) = match version {
            3 => (0, 0),
            5 => (w(2)?, w(3)?),
            v => return Err(format!("a hash segment of version {v}, which this check doesn't read (3 and 5)")),
        };
        let add = |a: usize, b: usize| a.checked_add(b).ok_or_else(|| "offset overflows".to_string());
        // The layout after the 40-byte header: hashes, Qualcomm's signature
        // and chain (version 5), the OEM's signature and chain.
        let q_at = add(add(40, hashes)?, sig_q)?;
        let oem_at = add(add(q_at, chain_q)?, sig)?;
        let part = |at: usize, len: usize| -> Result<&[u8], String> {
            seg.get(at..add(at, len)?)
                .ok_or_else(|| "cut short: the certificate chain ends beyond the hash segment".into())
        };
        Ok((version, part(oem_at, chain)?, part(q_at, chain_q)?))
    }

    /// One DER element at `at`: its tag, where its content starts and where it ends.
    fn tlv(b: &[u8], at: usize) -> Result<(u8, usize, usize), String> {
        let bad = || "not a well-formed certificate".to_string();
        let tag = *b.get(at).ok_or_else(bad)?;
        let first = *b.get(at + 1).ok_or_else(bad)?;
        let (len, start) = if first < 0x80 {
            (first as usize, at + 2)
        } else {
            let n = (first & 0x7f) as usize;
            if n == 0 || n > 4 {
                return Err(bad());
            }
            let mut len = 0usize;
            for i in 0..n {
                len = (len << 8) | *b.get(at + 2 + i).ok_or_else(bad)? as usize;
            }
            (len, at + 2 + n)
        };
        let end = start.checked_add(len).filter(|e| *e <= b.len()).ok_or_else(bad)?;
        Ok((tag, start, end))
    }

    /// The elements inside a constructed element's content.
    fn children(b: &[u8], start: usize, end: usize) -> Result<Vec<(u8, usize, usize, usize)>, String> {
        let mut out = Vec::new();
        let mut at = start;
        while at < end {
            let (tag, s, e) = tlv(&b[..end], at)?;
            out.push((tag, at, s, e));
            at = e;
        }
        Ok(out)
    }

    /// The certificates of a chain, in order: DER SEQUENCEs one after the
    /// other, then padding.
    fn certificates(chain: &[u8]) -> Result<Vec<&[u8]>, String> {
        let mut out = Vec::new();
        let mut at = 0;
        while at < chain.len() && chain[at] == 0x30 {
            let (_, _, end) = tlv(chain, at)?;
            out.push(&chain[at..end]);
            at = end;
        }
        if chain[at..].iter().any(|x| *x != 0xff && *x != 0) {
            return Err("something other than certificates in the certificate chain".into());
        }
        Ok(out)
    }

    /// A certificate's issuer and subject (DER of each Name).
    fn issuer_subject(cert: &[u8]) -> Result<(&[u8], &[u8]), String> {
        let bad = || "not a well-formed certificate".to_string();
        let (tag, s, e) = tlv(cert, 0)?;
        if tag != 0x30 {
            return Err(bad());
        }
        let top = children(cert, s, e)?;
        let &(tag, _, ts, te) = top.first().ok_or_else(bad)?;
        if tag != 0x30 {
            return Err(bad());
        }
        let tbs = children(cert, ts, te)?;
        // [0] version (optional), serial, signature algorithm, issuer, validity, subject.
        let skip = usize::from(tbs.first().is_some_and(|x| x.0 == 0xa0));
        let field = |i: usize| -> Result<&[u8], String> {
            let &(tag, at, _, end) = tbs.get(skip + i).ok_or_else(bad)?;
            if tag != 0x30 {
                return Err(bad());
            }
            Ok(&cert[at..end])
        };
        Ok((field(2)?, field(4)?))
    }

    /// The attestation certificate's organisational units of the form
    /// `NN VALUE NAME` (`02 <16 hex digits> HW_ID`, `13 0001
    /// IN_USE_SOC_HW_VERSION`, ...), by NAME.
    fn ou_fields(subject: &[u8]) -> Result<std::collections::BTreeMap<String, String>, String> {
        const OU: &[u8] = &[0x06, 0x03, 0x55, 0x04, 0x0b];
        let mut out = std::collections::BTreeMap::new();
        let (_, s, e) = tlv(subject, 0)?;
        for (_, _, ss, se) in children(subject, s, e)? {
            for (_, _, as_, ae) in children(subject, ss, se)? {
                let parts = children(subject, as_, ae)?;
                let [(_, oid_at, _, oid_end), (_, _, vs, ve)] = parts[..] else { continue };
                if &subject[oid_at..oid_end] != OU {
                    continue;
                }
                let Ok(text) = std::str::from_utf8(&subject[vs..ve]) else { continue };
                let words: Vec<&str> = text.split_whitespace().collect();
                if let [_, value, name] = words[..] {
                    out.insert(name.to_string(), value.to_string());
                }
            }
        }
        Ok(out)
    }

    /// Reads what a signed image says about itself, or why it isn't one.
    pub fn read(b: &[u8]) -> Result<Signed, String> {
        let phdrs = program_headers(b)?;
        let mut hash = phdrs.iter().filter(|p| p.flags & SEGMENT_TYPE_MASK == SEGMENT_TYPE_HASH);
        let seg = match (hash.next(), hash.next()) {
            (Some(p), None) => &b[p.offset..p.offset + p.filesz],
            (None, _) => return Err("no hash segment: not a signed image".into()),
            (Some(_), Some(_)) => return Err("more than one hash segment".into()),
        };
        let (version, oem, qcom) = chains(seg)?;
        let chain = if oem.is_empty() { qcom } else { oem };
        let certs = certificates(chain)?;
        if certs.len() < 2 {
            return Err("a certificate chain without an attestation and a root certificate".into());
        }
        let root = certs[certs.len() - 1];
        let (issuer, subject) = issuer_subject(root)?;
        if issuer != subject {
            return Err("the certificate chain doesn't end in a root certificate".into());
        }
        let (_, attest_subject) = issuer_subject(certs[0])?;
        let ou = ou_fields(attest_subject)?;
        let hw = ou
            .get("HW_ID")
            .filter(|id| id.len() == 16 && id.bytes().all(|c| c.is_ascii_hexdigit()))
            .map(|id| id.to_ascii_lowercase())
            .ok_or("no HW_ID in the attestation certificate")?;
        // With IN_USE_SOC_HW_VERSION set, the HW_ID's first half is the SoC's
        // hardware family, not an msm id: the chip is named as that, so it
        // never passes for an msm id.
        let soc_version = ou.get("IN_USE_SOC_HW_VERSION").is_some_and(|v| v.trim_start_matches('0') == "1");
        let chip = if soc_version {
            format!("soc-hw-version:{}", &hw[..4])
        } else {
            hw[..8].to_string()
        };
        Ok(Signed {
            version,
            chip,
            hw_id: hw,
            root_sha256: hex(&Sha256::digest(root)),
            root_sha384: hex(&Sha384::digest(root)),
            certificates: certs.len(),
        })
    }
}
