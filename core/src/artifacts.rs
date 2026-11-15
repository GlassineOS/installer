// SPDX-License-Identifier: GPL-2.0-or-later
//! Where the artifacts come from: one interface for a folder holding a
//! `release.json` and for a published release. The manifest names, per role,
//! the file, its SHA-256 and its size; nothing of an artifact is used before
//! it checks out. A whole-file check reads it once ([`Artifacts::verify`]);
//! a reader that hands the bytes on ([`Artifacts::reader`]) hashes them on
//! the way and fails the read that completes the file when the hash differs,
//! so a consumer that sends as it reads never sends the last piece of a
//! wrong file. Every error names the role.
//!
//! The source itself (files, HTTP, the browser's File API) is the edge's.

use crate::release::Release;
use crate::table::ReadAt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fmt;

/// The manifest's file name in every source.
pub const MANIFEST: &str = "release.json";
/// The largest manifest read.
const MANIFEST_MAX: u64 = 1 << 20;
/// The piece size of a whole-file check.
const PIECE: usize = 1 << 20;

/// The files of one release. Paths are the manifest's, relative, `/`-separated.
pub trait ArtifactSource {
    /// Where the release comes from, for messages (a folder, a release's name).
    fn origin(&self) -> String;
    /// The size of `file`, or why it can't be had.
    fn size(&mut self, file: &str) -> Result<u64, String>;
    /// Fills `buf` from `offset` of `file`; a short read is an error.
    fn read_at(&mut self, file: &str, offset: u64, buf: &mut [u8]) -> Result<(), String>;
}

/// A release held in memory (tests; a browser that has the files already).
#[derive(Debug, Clone, Default)]
pub struct MemorySource {
    pub origin: String,
    pub files: BTreeMap<String, Vec<u8>>,
}

impl ArtifactSource for MemorySource {
    fn origin(&self) -> String {
        self.origin.clone()
    }
    fn size(&mut self, file: &str) -> Result<u64, String> {
        self.files.get(file).map(|f| f.len() as u64).ok_or_else(|| format!("no file {file}"))
    }
    fn read_at(&mut self, file: &str, offset: u64, buf: &mut [u8]) -> Result<(), String> {
        let mut f: &[u8] = self.files.get(file).ok_or_else(|| format!("no file {file}"))?;
        f.read_at(offset, buf)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArtifactError {
    /// The source has no readable manifest.
    NoManifest {
        origin: String,
        message: String,
    },
    /// The manifest isn't one this installer reads.
    BadManifest {
        origin: String,
        message: String,
    },
    /// The release doesn't supply the role.
    MissingRole {
        role: String,
    },
    /// The manifest's entry for the role lacks a field, or it is malformed.
    BadEntry {
        role: String,
        message: String,
    },
    /// The role's file can't be read.
    Unreadable {
        role: String,
        file: String,
        message: String,
    },
    SizeMismatch {
        role: String,
        file: String,
        stated: u64,
        found: u64,
    },
    HashMismatch {
        role: String,
        file: String,
        stated: String,
        found: String,
    },
    /// A reader was asked for bytes out of order (it hashes as it goes).
    OutOfOrder {
        role: String,
        expected: u64,
        asked: u64,
    },
}

impl fmt::Display for ArtifactError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ArtifactError::NoManifest { origin, message } => write!(f, "{origin} has no {MANIFEST}: {message}"),
            ArtifactError::BadManifest { origin, message } => write!(f, "{origin}: {MANIFEST}: {message}"),
            ArtifactError::MissingRole { role } => write!(f, "the release has no artifact for {role}"),
            ArtifactError::BadEntry { role, message } => write!(f, "the release's entry for {role}: {message}"),
            ArtifactError::Unreadable { role, file, message } => write!(f, "{role} ({file}) can't be read: {message}"),
            ArtifactError::SizeMismatch { role, file, stated, found } => {
                write!(f, "{role} ({file}) is {found} bytes, the release states {stated}: not the release's file")
            }
            ArtifactError::HashMismatch { role, file, stated, found } => {
                write!(f, "{role} ({file}) has SHA-256 {found}, the release states {stated}: not the release's file")
            }
            ArtifactError::OutOfOrder { role, expected, asked } => {
                write!(f, "{role} was read at {asked}, expected {expected} (reads go in order)")
            }
        }
    }
}

impl From<ArtifactError> for String {
    fn from(e: ArtifactError) -> String {
        e.to_string()
    }
}

/// One role's artifact as the manifest states it, its entry checked.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Artifact {
    pub role: String,
    pub file: String,
    pub sha256: String,
    pub size: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
}

/// A file path the manifest may use: relative, `/`-separated, no `.`/`..`.
pub fn plain_path(p: &str) -> bool {
    !p.is_empty()
        && !p.starts_with('/')
        && !p.contains('\\')
        && !p.contains(':')
        && !p.contains('\0')
        && p.split('/').all(|c| !c.is_empty() && c != "." && c != "..")
}

fn hex(d: &[u8]) -> String {
    d.iter().map(|b| format!("{b:02x}")).collect()
}

/// A release opened from a source: its manifest read and parsed.
pub struct Artifacts<S: ArtifactSource> {
    source: S,
    pub manifest: Release,
}

impl<S: ArtifactSource> Artifacts<S> {
    pub fn open(mut source: S) -> Result<Self, ArtifactError> {
        let origin = source.origin();
        let no = |message: String| ArtifactError::NoManifest {
            origin: origin.clone(),
            message,
        };
        let n = source.size(MANIFEST).map_err(no)?;
        if n > MANIFEST_MAX {
            return Err(ArtifactError::BadManifest {
                origin,
                message: format!("{n} bytes is too large"),
            });
        }
        let mut buf = vec![0u8; n as usize];
        source.read_at(MANIFEST, 0, &mut buf).map_err(no)?;
        let text = String::from_utf8(buf).map_err(|_| ArtifactError::BadManifest {
            origin: origin.clone(),
            message: "not UTF-8".into(),
        })?;
        let manifest = Release::parse(&text).map_err(|message| ArtifactError::BadManifest { origin, message })?;
        Ok(Artifacts { source, manifest })
    }

    pub fn source(&self) -> &S {
        &self.source
    }

    /// The roles the release supplies a file for.
    pub fn roles(&self) -> Vec<String> {
        self.manifest
            .artifacts
            .iter()
            .filter(|(_, r)| r.file.is_some())
            .map(|(n, _)| n.clone())
            .collect()
    }

    /// What the manifest states for `role`, every field there and well formed.
    pub fn artifact(&self, role: &str) -> Result<Artifact, ArtifactError> {
        let r = self.manifest.artifacts.get(role).ok_or(ArtifactError::MissingRole { role: role.into() })?;
        let bad = |message: &str| ArtifactError::BadEntry {
            role: role.into(),
            message: message.into(),
        };
        let file = r.file.clone().ok_or_else(|| bad("no file"))?;
        if !plain_path(&file) {
            return Err(bad(&format!("'{file}' is not a plain relative path")));
        }
        let sha256 = r.sha256.clone().ok_or_else(|| bad("no sha256"))?;
        if sha256.len() != 64 || !sha256.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
            return Err(bad("sha256 is not 64 lower-case hex digits"));
        }
        let size = r.size.ok_or_else(|| bad("no size"))?;
        Ok(Artifact {
            role: role.into(),
            file,
            sha256,
            size,
            kind: r.kind.clone(),
        })
    }

    fn checked_size(&mut self, a: &Artifact) -> Result<(), ArtifactError> {
        let found = self.source.size(&a.file).map_err(|message| ArtifactError::Unreadable {
            role: a.role.clone(),
            file: a.file.clone(),
            message,
        })?;
        if found != a.size {
            return Err(ArtifactError::SizeMismatch {
                role: a.role.clone(),
                file: a.file.clone(),
                stated: a.size,
                found,
            });
        }
        Ok(())
    }

    /// Reads the whole file once and checks its size and hash. `progress`
    /// gets the bytes read so far.
    pub fn verify(&mut self, role: &str, progress: &mut dyn FnMut(u64)) -> Result<Artifact, ArtifactError> {
        let mut r = self.reader(role)?;
        let mut buf = vec![0u8; r.artifact.size.min(PIECE as u64) as usize];
        while r.at < r.artifact.size {
            let n = (r.artifact.size - r.at).min(buf.len() as u64) as usize;
            let at = r.at;
            r.read(at, &mut buf[..n])?;
            progress(r.at);
        }
        r.finish()
    }

    /// A reader of the role's file that checks it while it is read: in
    /// order from 0 (a read at 0 starts over), never past the end; the read
    /// that completes the file fails if the hash differs.
    pub fn reader(&mut self, role: &str) -> Result<VerifiedReader<'_, S>, ArtifactError> {
        let artifact = self.artifact(role)?;
        self.checked_size(&artifact)?;
        Ok(VerifiedReader {
            source: &mut self.source,
            artifact,
            at: 0,
            hash: Sha256::new(),
            checked: false,
        })
    }
}

pub struct VerifiedReader<'a, S: ArtifactSource> {
    source: &'a mut S,
    pub artifact: Artifact,
    at: u64,
    hash: Sha256,
    checked: bool,
}

impl<S: ArtifactSource> VerifiedReader<'_, S> {
    /// Bytes read (and hashed) so far.
    pub fn position(&self) -> u64 {
        self.at
    }

    fn read(&mut self, offset: u64, buf: &mut [u8]) -> Result<(), ArtifactError> {
        let a = &self.artifact;
        if offset == 0 && self.at != 0 {
            self.at = 0;
            self.hash = Sha256::new();
            self.checked = false;
        }
        if offset != self.at {
            return Err(ArtifactError::OutOfOrder {
                role: a.role.clone(),
                expected: self.at,
                asked: offset,
            });
        }
        let end = offset
            .checked_add(buf.len() as u64)
            .filter(|e| *e <= a.size)
            .ok_or_else(|| ArtifactError::Unreadable {
                role: a.role.clone(),
                file: a.file.clone(),
                message: format!("read of {} bytes at {offset} is past its end ({})", buf.len(), a.size),
            })?;
        self.source.read_at(&a.file, offset, buf).map_err(|message| ArtifactError::Unreadable {
            role: a.role.clone(),
            file: a.file.clone(),
            message,
        })?;
        self.hash.update(&*buf);
        self.at = end;
        if end == a.size {
            self.check()?;
        }
        Ok(())
    }

    fn check(&mut self) -> Result<(), ArtifactError> {
        let found = hex(&self.hash.clone().finalize());
        if found != self.artifact.sha256 {
            return Err(ArtifactError::HashMismatch {
                role: self.artifact.role.clone(),
                file: self.artifact.file.clone(),
                stated: self.artifact.sha256.clone(),
                found,
            });
        }
        self.checked = true;
        Ok(())
    }

    /// The artifact, once every byte was read and the hash matched.
    pub fn finish(mut self) -> Result<Artifact, ArtifactError> {
        if self.at == self.artifact.size && !self.checked {
            self.check()?; // an empty file: no read completed it
        }
        if self.at != self.artifact.size {
            return Err(ArtifactError::Unreadable {
                role: self.artifact.role.clone(),
                file: self.artifact.file.clone(),
                message: format!("only {} of {} bytes were read", self.at, self.artifact.size),
            });
        }
        Ok(self.artifact)
    }
}

impl<S: ArtifactSource> ReadAt for VerifiedReader<'_, S> {
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<(), String> {
        self.read(offset, buf).map_err(String::from)
    }
}
