// SPDX-License-Identifier: GPL-2.0-or-later
//! `emergency-try-program IMAGE`: the first upload of a GlassineOS programmer
//! (issue #18 step 3, `spikes/edl-programmer/TESTPLAN.md`). A development
//! command, not part of the guided flow.
//!
//! It offers IMAGE to the boot ROM in answer to its HELLO, serves its reads
//! (each logged), and after DONE_RESP listens on bulk IN for the program's
//! status record. It sends nothing after DONE. The image itself decides what
//! the device does next; the spike's image has no storage code.

use glassine_installer::fastboot::{FbError, Transport};
use glassine_installer::sahara::{DoneStatus, Sahara};
use std::time::{Duration, Instant};

/// Logs every transfer with the time since the first one: what the boot ROM
/// asks for, in order, is part of what the test learns.
pub struct Logged<T> {
    pub t: T,
    t0: Option<Instant>,
}

impl<T> Logged<T> {
    pub fn new(t: T) -> Self {
        Logged { t, t0: None }
    }
    fn at(&mut self) -> f64 {
        self.t0.get_or_insert_with(Instant::now).elapsed().as_secs_f64()
    }
}

fn words(d: &[u8]) -> String {
    d.chunks(4)
        .take(8)
        .map(|w| {
            let mut b = [0u8; 4];
            b[..w.len()].copy_from_slice(w);
            format!("{:x}", u32::from_le_bytes(b))
        })
        .collect::<Vec<_>>()
        .join(" ")
}

impl<T: Transport> Transport for Logged<T> {
    async fn send(&mut self, d: &[u8]) -> Result<(), FbError> {
        let at = self.at();
        let r = self.t.send(d).await;
        if d.len() <= 64 {
            eprintln!("{at:9.3} OUT {:>7} B  [{}] {}", d.len(), words(d), if r.is_ok() { "" } else { "ERR" });
        } else {
            eprintln!("{at:9.3} OUT {:>7} B  (image bytes) {}", d.len(), if r.is_ok() { "" } else { "ERR" });
        }
        r
    }
    async fn recv(&mut self) -> Result<Vec<u8>, FbError> {
        self.recv_max(4096).await
    }
    async fn recv_max(&mut self, max: usize) -> Result<Vec<u8>, FbError> {
        let r = self.t.recv_max(max).await;
        let at = self.at();
        match &r {
            Ok(d) => eprintln!("{at:9.3}  IN {:>7} B  [{}]", d.len(), words(d)),
            Err(e) => eprintln!("{at:9.3}  IN error: {e}"),
        }
        r
    }
}

/// How long the command listens on bulk IN after DONE_RESP.
pub const LISTEN: Duration = Duration::from_secs(20);

/// Runs the upload over `t`, then reads bulk IN for `listen`. Returns what
/// goes to stdout.
pub async fn run<T: Transport>(t: T, image: &[u8], listen: Duration) -> Result<String, String> {
    let mut out = String::new();
    let mut s = Sahara::connect(Logged::new(t)).await.map_err(|e| format!("waiting for HELLO: {e}"))?;
    let h = *s.hello();
    out += &format!("hello: version {} (oldest {}), mode {:?}\n", h.version, h.version_min, h.mode);
    let up = match s.upload(image).await {
        Ok(u) => u,
        Err(e) => {
            out += &format!("upload: REFUSED or failed: {e}\n");
            return Ok(out);
        }
    };
    out += &format!(
        "upload: image id {:#x}, {} reads, {} bytes, 64-bit reads {}, done {:?}\n",
        up.image_id, up.reads, up.bytes, up.wide, up.done
    );
    if up.done != DoneStatus::Complete {
        return Ok(out);
    }
    let t0 = Instant::now();
    let mut lt = s.into_transport();
    while t0.elapsed() < listen {
        match lt.t.recv_max(512).await {
            Ok(d) if d.is_empty() => continue,
            Ok(d) => {
                out += &format!("bulk IN at T0+{:.3}s: {} B\n", t0.elapsed().as_secs_f64(), d.len());
                for (i, w) in d.chunks(4).enumerate() {
                    let mut b = [0u8; 4];
                    b[..w.len()].copy_from_slice(w);
                    out += &format!("  word {i:2}: {:#010x}\n", u32::from_le_bytes(b));
                }
                if d.starts_with(b"GLSNPRG0") {
                    out += "  => GLSNPRG0: our code ran and drove the boot ROM's USB device\n";
                }
            }
            Err(FbError::Timeout(_)) => continue,
            Err(e) => {
                out += &format!("bulk IN at T0+{:.3}s ended: {e}\n", t0.elapsed().as_secs_f64());
                break;
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use glassine_installer::sahara::{self, Mode, cmd};
    use pollster::block_on;
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::rc::Rc;

    /// A scripted boot ROM: each read takes the next item (bytes or an
    /// error); an empty script times out. What the computer sends is kept.
    struct Rom {
        incoming: VecDeque<Result<Vec<u8>, FbError>>,
        sent: Rc<RefCell<Vec<Vec<u8>>>>,
    }

    impl Transport for Rom {
        async fn send(&mut self, d: &[u8]) -> Result<(), FbError> {
            self.sent.borrow_mut().push(d.to_vec());
            Ok(())
        }
        async fn recv(&mut self) -> Result<Vec<u8>, FbError> {
            self.recv_max(4096).await
        }
        async fn recv_max(&mut self, max: usize) -> Result<Vec<u8>, FbError> {
            let mut t = self.incoming.pop_front().unwrap_or(Err(FbError::Timeout("waiting for an answer".into())))?;
            if t.len() > max {
                let rest = t.split_off(max);
                self.incoming.push_front(Ok(rest));
            }
            Ok(t)
        }
    }

    fn pkt(c: u32, body: &[u32]) -> Result<Vec<u8>, FbError> {
        let mut v = c.to_le_bytes().to_vec();
        v.extend_from_slice(&((8 + 4 * body.len()) as u32).to_le_bytes());
        for w in body {
            v.extend_from_slice(&w.to_le_bytes());
        }
        Ok(v)
    }

    const ID: u32 = 0x0d;

    /// A stand-in image: no real program, just bytes the reads can check.
    fn image() -> Vec<u8> {
        (0..0x200u32).map(|i| (i * 7) as u8).collect()
    }

    /// HELLO, then the boot ROM reads the ELF header and the program
    /// headers, as it does before it checks anything.
    fn opening() -> Vec<Result<Vec<u8>, FbError>> {
        vec![
            pkt(cmd::HELLO, &[2, 1, 0x400, 0, 0, 0, 0, 0, 0, 0]),
            pkt(cmd::READ_DATA, &[ID, 0, 0x34]),
            pkt(cmd::READ_DATA, &[ID, 0x40, 0x70]),
        ]
    }

    /// What the computer must send for [`opening`]: the answer to HELLO
    /// choosing image transfer and the two pieces of the image.
    fn opening_sent() -> Vec<Vec<u8>> {
        let img = image();
        vec![sahara::hello_resp(Mode::ImageTxPending), img[..0x34].to_vec(), img[0x40..0xb0].to_vec()]
    }

    /// Short enough for a test, long enough for the loop to read every
    /// scripted item (the fake answers at once).
    const LISTEN: Duration = Duration::from_millis(50);

    fn go(script: Vec<Result<Vec<u8>, FbError>>) -> (String, Vec<Vec<u8>>) {
        let sent = Rc::new(RefCell::new(Vec::new()));
        let rom = Rom {
            incoming: script.into(),
            sent: sent.clone(),
        };
        let out = block_on(run(rom, &image(), LISTEN)).unwrap();
        let s = sent.borrow().clone();
        (out, s)
    }

    /// An accepted image: END_IMAGE_TX success, then DONE_RESP complete.
    fn accepted() -> Vec<Result<Vec<u8>, FbError>> {
        let mut s = opening();
        s.push(pkt(cmd::END_IMAGE_TX, &[ID, 0]));
        s.push(pkt(cmd::DONE_RESP, &[1]));
        s
    }

    fn accepted_sent() -> Vec<Vec<u8>> {
        let mut s = opening_sent();
        s.push(sahara::done());
        s
    }

    #[test]
    fn refused_after_the_program_headers() {
        let mut s = opening();
        s.push(pkt(cmd::END_IMAGE_TX, &[ID, 0x28]));
        let (out, sent) = go(s);
        assert!(out.starts_with("hello: version 2 (oldest 1)"), "{out}");
        assert!(out.contains("upload: REFUSED or failed: the device refused the image: "), "{out}");
        assert!(out.contains("(0x28)"), "{out}");
        assert!(out.contains("hardware bulk transfer error"), "{out}");
        assert!(!out.contains("bulk IN"), "no listening after a refusal: {out}");
        // No DONE after a refusal: only the answer to HELLO and the two reads.
        assert_eq!(sent, opening_sent());
    }

    #[test]
    fn accepted_and_the_program_reports() {
        let mut s = accepted();
        let mut rec = b"GLSNPRG0".to_vec();
        rec.extend_from_slice(&0x1122_3344u32.to_le_bytes());
        rec.resize(128, 0);
        s.push(Ok(rec));
        let (out, sent) = go(s);
        assert!(
            out.contains("upload: image id 0xd, 2 reads, 164 bytes, 64-bit reads false, done Complete\n"),
            "{out}"
        );
        assert!(out.contains(": 128 B\n"), "{out}");
        // "GLSN" and "PRG0" as little-endian words, then the next word.
        assert!(out.contains("  word  0: 0x4e534c47\n"), "{out}");
        assert!(out.contains("  word  1: 0x30475250\n"), "{out}");
        assert!(out.contains("  word  2: 0x11223344\n"), "{out}");
        assert!(out.contains("  word 31: 0x00000000\n"), "{out}");
        assert!(!out.contains("word 32"), "{out}");
        assert!(out.contains("  => GLSNPRG0: our code ran"), "{out}");
        assert_eq!(sent, accepted_sent());
    }

    #[test]
    fn accepted_then_bulk_in_fails() {
        let mut s = accepted();
        s.push(Err(FbError::Usb("pipe error".into())));
        // Never read: the listening ends at the error.
        s.push(Ok(b"GLSNPRG0".to_vec()));
        let (out, sent) = go(s);
        let line = out.lines().find(|l| l.starts_with("bulk IN at T0+")).expect(&out);
        assert!(line.contains("s ended: ") && line.ends_with("pipe error"), "{line}");
        assert!(!out.contains("GLSNPRG0"), "{out}");
        assert_eq!(sent, accepted_sent());
    }

    /// The whole exchange, byte for byte: nothing is sent after DONE, also
    /// while listening, also when the program sends nothing or something
    /// else than its record.
    #[test]
    fn nothing_is_sent_after_done() {
        let (out, sent) = go(accepted());
        assert!(!out.contains("bulk IN"), "{out}");
        assert_eq!(sent, accepted_sent());
        let mut s = accepted();
        s.push(Ok(vec![0xee; 16]));
        s.push(Ok(vec![]));
        s.push(Err(FbError::Timeout("waiting for an answer".into())));
        let (out, sent) = go(s);
        assert!(out.contains(": 16 B\n  word  0: 0xeeeeeeee\n"), "{out}");
        assert!(!out.contains("=> GLSNPRG0"), "{out}");
        assert_eq!(sent, accepted_sent());
        assert_eq!(sent.last().unwrap(), &sahara::done());
        assert_eq!(sent.iter().filter(|p| **p == sahara::done()).count(), 1);
    }

    /// A DONE_RESP that isn't "complete": no listening.
    #[test]
    fn pending_done_does_not_listen() {
        let mut s = opening();
        s.push(pkt(cmd::END_IMAGE_TX, &[ID, 0]));
        s.push(pkt(cmd::DONE_RESP, &[0]));
        s.push(Ok(b"GLSNPRG0".to_vec()));
        let (out, sent) = go(s);
        assert!(out.ends_with("done Pending\n"), "{out}");
        assert_eq!(sent, accepted_sent());
    }
}
