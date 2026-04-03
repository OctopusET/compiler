/*
 * Direct packfile writer for bare git repositories.
 *
 * Generates a single packfile instead of loose objects.
 * Supports nested tree paths (e.g., kr/group/file.md).
 *
 * Each commit updates one blob in one group subtree.
 * Only the changed subtree is re-serialized; all others
 * subtrees keep their SHA.
 */

use std::fs::{self, File};
use std::io::{BufWriter, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};
use sha1::{Digest, Sha1};

const BOT_NAME: &str = "legalize-kr-bot";
const BOT_EMAIL: &str = "bot@legalize.kr";

struct Entry {
    name: Vec<u8>,
    sha: [u8; 20],
    is_tree: bool,
}

struct Group {
    name: Vec<u8>,
    files: Vec<Entry>,
    cached_sha: Option<[u8; 20]>,
}

pub struct PackRepoWriter {
    pw: PackWriter,
    root_files: Vec<Entry>,
    groups: Vec<Group>,
    parent: Option<[u8; 20]>,
    output: PathBuf,
}

impl PackRepoWriter {
    pub fn create(output: &Path) -> Result<Self> {
        if output.exists() {
            fs::remove_dir_all(output)?;
        }
        let r = Command::new("git").args(["init", "--bare"]).arg(output)
            .output().context("git init")?;
        if !r.status.success() {
            anyhow::bail!("git init: {}", String::from_utf8_lossy(&r.stderr));
        }
        let pp = output.join("objects/pack/tmp_pack.pack");
        fs::create_dir_all(pp.parent().unwrap())?;

        Ok(Self {
            pw: PackWriter::new(&pp)?,
            root_files: Vec::new(),
            groups: Vec::new(),
            parent: None,
            output: output.to_path_buf(),
        })
    }

    pub fn commit_law(
        &mut self, path: &str, md: &[u8], msg: &str, prom_date: &str,
    ) -> Result<()> {
        let (epoch, tz) = commit_time(prom_date);
        self.commit(path, md, msg, epoch, tz)
    }

    pub fn commit_static(
        &mut self, path: &str, data: &[u8], msg: &str, epoch: i64, tz: i32,
    ) -> Result<()> {
        self.commit_with_author(path, data, msg, epoch, tz, None, None)
    }

    pub fn commit_static_authored(
        &mut self, path: &str, data: &[u8], msg: &str, epoch: i64, tz: i32,
        author: &str, committer: &str,
    ) -> Result<()> {
        self.commit_with_author(path, data, msg, epoch, tz,
                                Some(author.to_owned()), Some(committer.to_owned()))
    }

    pub fn commit(
        &mut self, path: &str, content: &[u8], msg: &str, epoch: i64, tz: i32,
    ) -> Result<()> {
        self.commit_with_author(path, content, msg, epoch, tz, None, None)
    }

    fn commit_with_author(
        &mut self, path: &str, content: &[u8], msg: &str, epoch: i64, tz: i32,
        author_override: Option<String>, committer_override: Option<String>,
    ) -> Result<()> {
        let blob_sha = self.pw.write_obj(3, content)?;
        let parts: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();

        match parts.len() {
            1 => {
                upsert(&mut self.root_files, parts[0].as_bytes(), blob_sha, false);
            }
            3 if parts[0] == "kr" => {
                let gname = parts[1].as_bytes();
                let fname = parts[2].as_bytes();
                let gi = self.ensure_group(gname);
                upsert(&mut self.groups[gi].files, fname, blob_sha, false);
                self.groups[gi].cached_sha = None; /* invalidate */
            }
            _ => anyhow::bail!("unsupported path: {path}"),
        }

        let root_sha = self.build_root_tree()?;
        let c = self.make_commit(root_sha, msg, epoch, tz,
                                 author_override.as_deref(),
                                 committer_override.as_deref())?;
        self.parent = Some(c);
        Ok(())
    }

    pub fn finish(mut self) -> Result<()> {
        self.pw.finish()?;

        if let Some(sha) = self.parent {
            let rd = self.output.join("refs/heads");
            fs::create_dir_all(&rd)?;
            fs::write(rd.join("main"), format!("{}\n", hex(&sha)))?;
        }
        fs::write(self.output.join("HEAD"), "ref: refs/heads/main\n")?;

        let pp = self.output.join("objects/pack/tmp_pack.pack");
        let r = Command::new("git").arg("index-pack").arg(&pp)
            .output().context("git index-pack")?;
        if !r.status.success() {
            anyhow::bail!("index-pack: {}", String::from_utf8_lossy(&r.stderr));
        }
        Ok(())
    }

    fn ensure_group(&mut self, name: &[u8]) -> usize {
        if let Some(i) = self.groups.iter().position(|g| g.name == name) {
            return i;
        }
        let pos = self.groups.partition_point(|g| g.name.as_slice() < name);
        self.groups.insert(pos, Group {
            name: name.to_vec(),
            files: Vec::new(),
            cached_sha: None,
        });
        pos
    }

    fn build_root_tree(&mut self) -> Result<[u8; 20]> {
        /* 1. Ensure every group has a cached subtree SHA */
        for g in &mut self.groups {
            if g.cached_sha.is_some() {
                continue;
            }
            let buf = tree_bytes(&g.files);
            let sha = git_hash(b"tree", &buf);
            self.pw.write_obj(2, &buf)?;
            g.cached_sha = Some(sha);
        }

        /* 2. Build kr/ tree */
        let mut kr_buf = Vec::with_capacity(self.groups.len() * 80);
        for g in &self.groups {
            kr_buf.extend_from_slice(b"40000 ");
            kr_buf.extend_from_slice(&g.name);
            kr_buf.push(0);
            kr_buf.extend_from_slice(&g.cached_sha.unwrap());
        }
        let kr_sha = git_hash(b"tree", &kr_buf);
        self.pw.write_obj(2, &kr_buf)?;

        /* 3. Root tree: root_files + kr directory, sorted by git rules */
        let mut root = Vec::<(&[u8], [u8; 20], bool)>::new();
        for e in &self.root_files {
            root.push((&e.name, e.sha, false));
        }
        if !self.groups.is_empty() {
            root.push((b"kr", kr_sha, true));
        }
        root.sort_by(|a, b| sort_key(a.0, a.2).cmp(&sort_key(b.0, b.2)));

        let mut root_buf = Vec::new();
        for (name, sha, is_tree) in &root {
            root_buf.extend_from_slice(if *is_tree { b"40000 " } else { b"100644 " });
            root_buf.extend_from_slice(name);
            root_buf.push(0);
            root_buf.extend_from_slice(sha);
        }
        let root_sha = git_hash(b"tree", &root_buf);
        self.pw.write_obj(2, &root_buf)?;
        Ok(root_sha)
    }

    fn make_commit(
        &mut self, tree: [u8; 20], msg: &str, epoch: i64, tz: i32,
        author_override: Option<&str>, committer_override: Option<&str>,
    ) -> Result<[u8; 20]> {
        let sign = if tz >= 0 { '+' } else { '-' };
        let a = tz.unsigned_abs();
        let tz_str = format!("{sign}{:02}{:02}", a / 60, a % 60);
        let default_id = format!("{BOT_NAME} <{BOT_EMAIL}>");
        let author_id = author_override.unwrap_or(&default_id);
        let committer_id = committer_override.unwrap_or(&default_id);

        let mut buf = format!("tree {}\n", hex(&tree));
        if let Some(p) = self.parent {
            buf.push_str(&format!("parent {}\n", hex(&p)));
        }
        buf.push_str(&format!("author {author_id} {epoch} {tz_str}\n"));
        buf.push_str(&format!("committer {committer_id} {epoch} {tz_str}\n"));
        buf.push_str(&format!("\n{msg}"));
        self.pw.write_obj(1, buf.as_bytes())
    }
}

/* --- helpers --- */

fn upsert(v: &mut Vec<Entry>, name: &[u8], sha: [u8; 20], is_tree: bool) {
    match v.iter().position(|e| e.name == name) {
        Some(i) => v[i].sha = sha,
        None => {
            let p = v.partition_point(|e| e.name.as_slice() < name);
            v.insert(p, Entry { name: name.to_vec(), sha, is_tree });
        }
    }
}

fn tree_bytes(entries: &[Entry]) -> Vec<u8> {
    let mut buf = Vec::new();
    for e in entries {
        buf.extend_from_slice(if e.is_tree { b"40000 " } else { b"100644 " });
        buf.extend_from_slice(&e.name);
        buf.push(0);
        buf.extend_from_slice(&e.sha);
    }
    buf
}

fn sort_key(name: &[u8], is_tree: bool) -> Vec<u8> {
    let mut k = name.to_vec();
    if is_tree { k.push(b'/'); }
    k
}

fn git_hash(typename: &[u8], data: &[u8]) -> [u8; 20] {
    let hdr = format!("{} {}\0", std::str::from_utf8(typename).unwrap(), data.len());
    let mut h = Sha1::new();
    h.update(hdr.as_bytes());
    h.update(data);
    h.finalize().into()
}

fn hex(sha: &[u8; 20]) -> String {
    sha.iter().map(|b| format!("{b:02x}")).collect()
}

fn compress(data: &[u8]) -> Vec<u8> {
    use flate2::write::ZlibEncoder;
    use flate2::Compression;
    let mut e = ZlibEncoder::new(Vec::new(), Compression::fast());
    e.write_all(data).unwrap();
    e.finish().unwrap()
}


fn commit_time(date: &str) -> (i64, i32) {
    let d = if date.len() == 8 && date.bytes().all(|b| b.is_ascii_digit()) && date >= "19700101" {
        date
    } else if date.len() == 8 && date.bytes().all(|b| b.is_ascii_digit()) {
        "19700101"
    } else {
        "20000101"
    };
    let y: i32 = d[0..4].parse().unwrap_or(2000);
    let m: u32 = d[4..6].parse().unwrap_or(1);
    let day: u32 = d[6..8].parse().unwrap_or(1);
    (days_since_epoch(y, m, day) * 86400 + 3 * 3600, 540)
}

fn days_since_epoch(y: i32, m: u32, d: u32) -> i64 {
    let (mut y, mut m) = (y as i64, m as i64);
    if m <= 2 { y -= 1; m += 12; }
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * (m - 3) + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/* --- packfile writer --- */

struct PackWriter {
    f: BufWriter<File>,
    sha: Sha1,
    n: u32,
    path: PathBuf,
}

impl PackWriter {
    fn new(path: &Path) -> Result<Self> {
        let f = BufWriter::with_capacity(1 << 20, File::create(path)?);
        let mut w = Self { f, sha: Sha1::new(), n: 0, path: path.to_path_buf() };
        w.raw(b"PACK")?;
        w.raw(&2u32.to_be_bytes())?;
        w.raw(&0u32.to_be_bytes())?;
        Ok(w)
    }

    fn raw(&mut self, d: &[u8]) -> Result<()> {
        self.f.write_all(d)?;
        self.sha.update(d);
        Ok(())
    }

    fn write_obj(&mut self, otype: u8, data: &[u8]) -> Result<[u8; 20]> {
        let sha = git_hash(type_str(otype), data);
        let sz = data.len();
        let mut hdr = ((otype & 7) << 4) | (sz & 0xf) as u8;
        let mut rem = sz >> 4;
        if rem > 0 { hdr |= 0x80; }
        self.raw(&[hdr])?;
        while rem > 0 {
            let mut b = (rem & 0x7f) as u8;
            rem >>= 7;
            if rem > 0 { b |= 0x80; }
            self.raw(&[b])?;
        }
        self.raw(&compress(data))?;
        self.n += 1;
        Ok(sha)
    }

    fn finish(&mut self) -> Result<()> {
        self.f.flush()?;
        let cnt = self.n.to_be_bytes();
        {
            let mut f = File::options().write(true).open(&self.path)?;
            f.seek(SeekFrom::Start(8))?;
            f.write_all(&cnt)?;
            f.flush()?;
            /* drop f before re-reading the file */
        }
        let data = fs::read(&self.path)?;
        let cksum: [u8; 20] = { let mut h = Sha1::new(); h.update(&data); h.finalize().into() };
        fs::OpenOptions::new().append(true).open(&self.path)?.write_all(&cksum)?;
        Ok(())
    }
}

fn type_str(t: u8) -> &'static [u8] {
    match t { 1 => b"commit", 2 => b"tree", 3 => b"blob", _ => panic!("invalid object type {t}") }
}
