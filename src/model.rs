use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

pub const ROOT: u64 = 1;
pub const CHUNK_SIZE: u64 = 64 * 1024;
pub const MAX_FILE_SIZE: u64 = 1024 * 1024 * 1024;
pub const MAX_IO_SIZE: u64 = 16 * 1024 * 1024;

#[derive(Debug)]
pub struct FsError(pub i32, pub &'static str);
impl std::fmt::Display for FsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.1)
    }
}
impl std::error::Error for FsError {}
pub fn require(condition: bool, errno: i32, message: &'static str) -> anyhow::Result<()> {
    if condition {
        Ok(())
    } else {
        Err(FsError(errno, message).into())
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    File,
    Directory,
    Symlink,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Node {
    pub id: u64,
    pub kind: Kind,
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
    pub nlink: u32,
    pub size: u64,
    pub atime_ns: u64,
    pub mtime_ns: u64,
    pub ctime_ns: u64,
    pub parent: u64,
    pub chunks: BTreeMap<u64, String>,
    pub target: Vec<u8>,
}

impl Node {
    pub fn new(id: u64, kind: Kind, parent: u64, mode: u32, uid: u32, gid: u32) -> Self {
        let now = now_ns();
        Self {
            id,
            kind,
            mode: mode & 0o7777,
            uid,
            gid,
            nlink: 1,
            size: 0,
            atime_ns: now,
            mtime_ns: now,
            ctime_ns: now,
            parent,
            chunks: BTreeMap::new(),
            target: vec![],
        }
    }
    pub fn metadata_fingerprint(&self) -> String {
        digest_json(&(
            self.id,
            self.kind,
            self.mode,
            self.uid,
            self.gid,
            self.nlink,
            self.size,
            self.atime_ns,
            self.mtime_ns,
            self.ctime_ns,
            self.parent,
            self.chunks.len(),
        ))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum Value {
    Node(Node),
    Dentry(u64),
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ObservationKind {
    Value,
    Metadata,
    Content,
    Directory,
    Write,
}

impl ObservationKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Value => "value",
            Self::Metadata => "metadata",
            Self::Content => "content",
            Self::Directory => "directory",
            Self::Write => "write",
        }
    }
    pub fn parse(s: &str) -> anyhow::Result<Self> {
        Ok(match s {
            "value" => Self::Value,
            "metadata" => Self::Metadata,
            "content" => Self::Content,
            "directory" => Self::Directory,
            "write" => Self::Write,
            _ => anyhow::bail!("unknown observation kind: {s}"),
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Observation {
    pub kind: ObservationKind,
    pub key: String,
    pub expected: String,
    pub path: String,
    pub operation: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Conflict {
    #[serde(flatten)]
    pub observation: Observation,
    pub observed: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Receipt {
    pub format: String,
    pub branch: String,
    pub base_revision: i64,
    pub checked_revision: i64,
    pub accepted_revision: Option<i64>,
    pub outcome: String,
    pub observations_checked: usize,
    pub keys_written: usize,
    pub conflicts: Vec<Conflict>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Branch {
    pub id: String,
    pub base_revision: i64,
    pub state: String,
}

pub fn now_ns() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
        .min(u64::MAX as u128) as u64
}

pub fn hash(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}
pub fn digest_json(value: &impl Serialize) -> String {
    hash(&serde_json::to_vec(value).expect("serializable value"))
}
pub fn node_key(id: u64) -> String {
    format!("n/{id:016x}")
}
pub fn dir_prefix(id: u64) -> String {
    format!("d/{id:016x}/")
}
pub fn dent_key(parent: u64, name: &[u8]) -> String {
    format!("{}{}", dir_prefix(parent), hex(name))
}

pub fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(DIGITS[(b >> 4) as usize] as char);
        s.push(DIGITS[(b & 15) as usize] as char);
    }
    s
}
pub fn unhex(s: &str) -> anyhow::Result<Vec<u8>> {
    anyhow::ensure!(s.len().is_multiple_of(2), "odd hex length");
    s.as_bytes()
        .chunks_exact(2)
        .map(|c| {
            let a = (c[0] as char)
                .to_digit(16)
                .ok_or_else(|| anyhow::anyhow!("bad hex"))?;
            let b = (c[1] as char)
                .to_digit(16)
                .ok_or_else(|| anyhow::anyhow!("bad hex"))?;
            Ok(((a << 4) | b) as u8)
        })
        .collect()
}

pub fn validate_name(name: &[u8]) -> anyhow::Result<()> {
    anyhow::ensure!(
        !name.is_empty()
            && name.len() <= 255
            && name != b"."
            && name != b".."
            && !name.contains(&b'/')
            && !name.contains(&0),
        "invalid directory entry name"
    );
    Ok(())
}

pub fn validate_branch(name: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        !name.is_empty()
            && name.len() <= 80
            && name
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_'),
        "invalid branch name"
    );
    Ok(())
}
