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
