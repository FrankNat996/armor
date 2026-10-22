//! Translations kept on disk and reused by later runs (ADR-0048).
//!
//! A translated block does not depend on where it is published (the
//! prologue, the epilogue and the flag routine are shared and reached
//! through the `Context`; exits are relative branches or table lookups;
//! the fault table holds offsets), only on its guest bytes, its guest
//! address and the translation options. So a record holds the block's
//! address, a key for the options (`Jit::cache_key`), the guest bytes it
//! was translated from, and its words and fault table as translated (exits
//! not yet chained). A block is reused only when the guest bytes now at its
//! address are exactly the recorded ones: the cache never decides that two
//! translations are equivalent.
//!
//! One file per program (`configure`: the path names armor's own build,
//! the program and its mode). The first lookup reads it and indexes its
//! records by their headers; a record is checked (its checksum) and decoded
//! only when a lookup needs it, and a damaged one is never run. New records
//! go to the end of the file when the process exits or replaces itself
//! (`save`), in one `write` with `O_APPEND`, so that armor processes
//! running the same program at once do not overwrite each other's; a file
//! grown past `MAX_FILE` starts over.

use std::collections::HashMap;
use std::fs;
use std::hash::{BuildHasherDefault, Hasher};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

const MAGIC: &[u8; 8] = b"ARMORJC2";
/// Records kept for one address and key (code that changes at the same
/// address, as a guest JIT's, would otherwise grow without end).
const PER_ADDRESS: usize = 4;
/// A cache file larger than this is dropped when the next records are
/// saved, and starts over.
const MAX_FILE: u64 = 256 << 20;
/// The cache directory's files together at most: past it, `save` removes
/// the files used least recently (by modification time, which `lookup`
/// renews when it reads a file).
const MAX_DIR: u64 = 1 << 30;
/// Records not yet saved, at most: a guest that keeps generating code (a
/// JIT of its own) stops adding past it.
const MAX_NEW: usize = 64 << 20;
/// Bytes of a record before its body: the body's length (u32), its
/// checksum (u64).
const HEADER: usize = 12;
/// The body's fixed part: rip, key (u64), the instruction count, the number
/// of guest bytes, of words and of faults (u32 each).
const FIXED: usize = 32;

/// A translation found in the cache, as its record holds it (`lookup`).
pub struct Hit<'a> {
    body: &'a [u8],
}

impl Hit<'_> {
    /// The translation's words, little-endian, 4 bytes each.
    pub fn words(&self) -> &[u8] {
        let (nb, nw) = (
            u32_at(self.body, 20) as usize,
            u32_at(self.body, 24) as usize,
        );
        &self.body[FIXED + nb..FIXED + nb + 4 * nw]
    }

    /// Its fault table, as (load, slow path) offsets in words.
    pub fn faults(&self) -> impl Iterator<Item = (u32, u32)> + '_ {
        let (nb, nw, nf) = (
            u32_at(self.body, 20) as usize,
            u32_at(self.body, 24) as usize,
            u32_at(self.body, 28) as usize,
        );
        let at = FIXED + nb + 4 * nw;
        (0..nf).map(move |i| {
            let f = at + 8 * i;
            (u32_at(self.body, f), u32_at(self.body, f + 4))
        })
    }

    /// The guest instructions it translates.
    pub fn instructions(&self) -> u32 {
        u32_at(self.body, 16)
    }
}

/// Where a record is: in the file read at the first lookup, or added in
/// this process (its offset in `State::new`).
#[derive(Clone, Copy)]
enum Place {
    File(usize),
    New(usize),
}

#[derive(Default)]
struct KeyHasher(u64);

impl Hasher for KeyHasher {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.write_u8(b);
        }
    }
    fn write_u64(&mut self, n: u64) {
        self.0 = (self.0 ^ n)
            .wrapping_mul(0x9e37_79b9_7f4a_7c15)
            .rotate_left(29);
    }
}

type Index = HashMap<(u64, u64), Places, BuildHasherDefault<KeyHasher>>;

/// The records kept for one address and key, oldest first: at most
/// `PER_ADDRESS`, in place (an allocation for each was a tenth of the
/// index's time in a warm Python startup).
#[derive(Clone, Copy)]
struct Places {
    items: [Place; PER_ADDRESS],
    len: usize,
}

impl Default for Places {
    fn default() -> Places {
        Places {
            items: [Place::New(0); PER_ADDRESS],
            len: 0,
        }
    }
}

impl Places {
    fn as_slice(&self) -> &[Place] {
        &self.items[..self.len]
    }

    /// Keeps the file's records only (`State::own`).
    fn keep_file_records(&mut self) {
        let mut kept = 0;
        for i in 0..self.len {
            if matches!(self.items[i], Place::File(_)) {
                self.items[kept] = self.items[i];
                kept += 1;
            }
        }
        self.len = kept;
    }
}

struct State {
    path: PathBuf,
    loaded: bool,
    /// The file as read.
    data: Vec<u8>,
    /// File records found damaged (their checksum): never used.
    damaged: Vec<usize>,
    index: Index,
    /// Records added in this process, encoded one after the other as in
    /// the file.
    new: Vec<u8>,
    /// Where the file's last whole record ends, when bytes follow it (a
    /// write cut short): `save` cuts them before appending.
    tail: Option<usize>,
}

impl State {
    /// Drops what a forked child inherited of its parent's unsaved
    /// records (`FORKED`).
    fn own(&mut self) {
        if FORKED.swap(false, Ordering::Relaxed) {
            self.new.clear();
            for places in self.index.values_mut() {
                places.keep_file_records();
            }
        }
    }
}

static STATE: Mutex<Option<State>> = Mutex::new(None);
/// Set in the child of a `fork` (`pthread_atfork`), which inherits the
/// parent's unsaved records: they are the parent's to save. A flag rather
/// than a comparison of pids, as `getpid` at every lookup was a syscall
/// per block translated.
static FORKED: AtomicBool = AtomicBool::new(false);

extern "C" fn forked() {
    FORKED.store(true, Ordering::Relaxed);
}
static HITS: AtomicU64 = AtomicU64::new(0);
static STORED: AtomicU64 = AtomicU64::new(0);

/// Turns the cache on, with its file at `path` (`armor`'s `main`).
pub fn configure(path: PathBuf) {
    static ATFORK: std::sync::Once = std::sync::Once::new();
    // SAFETY: registers a handler that only stores to an atomic, which
    // is async-signal-safe as a child handler must be.
    ATFORK.call_once(|| unsafe {
        libc::pthread_atfork(None, None, Some(forked));
    });
    *STATE.lock().expect("cache lock") = Some(State {
        path,
        loaded: false,
        data: Vec::new(),
        damaged: Vec::new(),
        index: Index::default(),
        new: Vec::new(),
        tail: None,
    });
}

/// Whether the cache is on.
pub fn enabled() -> bool {
    STATE.lock().expect("cache lock").is_some()
}

/// Blocks reused from the cache, and blocks recorded in it, so far.
pub fn counts() -> (u64, u64) {
    (HITS.load(Ordering::Relaxed), STORED.load(Ordering::Relaxed))
}

/// The translation of the block at `rip` for `key`, if the cache has one
/// whose guest bytes are those `fetch` reads now: given to `then` as it is
/// in the record, which no copy is made of (with the cache's lock held).
pub fn lookup<R>(
    rip: u64,
    key: u64,
    fetch: impl Fn(u64, &mut [u8]) -> usize,
    then: impl FnOnce(Hit<'_>) -> R,
) -> Option<R> {
    let mut guard = STATE.lock().expect("cache lock");
    let state = guard.as_mut()?;
    state.own();
    if !state.loaded {
        state.loaded = true;
        state.data = fs::read(&state.path).unwrap_or_default();
        // Used now: `trim` keeps the files used most recently.
        let _ = fs::File::options()
            .write(true)
            .open(&state.path)
            .and_then(|f| f.set_modified(std::time::SystemTime::now()));
        let (records, end) = scan(&state.data);
        for (rip, key, at) in records {
            push(&mut state.index, rip, key, Place::File(at));
        }
        if end > 0 && end < state.data.len() {
            state.tail = Some(end);
        }
    }
    let places = *state.index.get(&(rip, key))?;
    let mut found = None;
    for &place in places.as_slice().iter().rev() {
        let record: &[u8] = match place {
            Place::File(at) => {
                if state.damaged.contains(&at) {
                    continue;
                }
                let record = record_at(&state.data, at);
                if !intact(record) {
                    state.damaged.push(at);
                    continue;
                }
                record
            }
            Place::New(at) => record_at(&state.new, at),
        };
        let body = &record[HEADER..];
        if same_bytes(guest_bytes(body), rip, &fetch) {
            found = Some(place);
            break;
        }
    }
    let record = match found? {
        Place::File(at) => record_at(&state.data, at),
        Place::New(at) => record_at(&state.new, at),
    };
    HITS.fetch_add(1, Ordering::Relaxed);
    Some(then(Hit {
        body: &record[HEADER..],
    }))
}

/// Whether the guest bytes at `rip` are `bytes`, read a piece at a time.
fn same_bytes(bytes: &[u8], rip: u64, fetch: &impl Fn(u64, &mut [u8]) -> usize) -> bool {
    let mut now = [0u8; 256];
    let mut done = 0;
    while done < bytes.len() {
        let n = (bytes.len() - done).min(now.len());
        if fetch(rip + done as u64, &mut now[..n]) != n || now[..n] != bytes[done..done + n] {
            return false;
        }
        done += n;
    }
    true
}

/// Records a new translation of the block at `rip` (guest `bytes`, `n`
/// guest instructions).
pub fn insert(rip: u64, key: u64, bytes: &[u8], words: &[u32], faults: &[(u32, u32)], n: usize) {
    let mut guard = STATE.lock().expect("cache lock");
    let Some(state) = guard.as_mut() else {
        return;
    };
    state.own();
    if state.new.len() > MAX_NEW {
        return;
    }
    let at = state.new.len();
    encode(&mut state.new, rip, key, n as u32, bytes, words, faults);
    push(&mut state.index, rip, key, Place::New(at));
    STORED.fetch_add(1, Ordering::Relaxed);
}

/// Keeps `place` for (`rip`, `key`), the oldest dropped beyond
/// `PER_ADDRESS` (lookups try the newest first).
fn push(index: &mut Index, rip: u64, key: u64, place: Place) {
    let places = index.entry((rip, key)).or_default();
    if places.len == PER_ADDRESS {
        places.items.copy_within(1.., 0);
        places.len -= 1;
    }
    places.items[places.len] = place;
    places.len += 1;
}

/// Appends the records added in this process to the file. Errors leave it
/// as it was: the cache is an optimization.
pub fn save() {
    let mut guard = STATE.lock().expect("cache lock");
    let Some(state) = guard.as_mut() else {
        return;
    };
    state.own();
    if state.new.is_empty() {
        return;
    }
    if let Some(dir) = state.path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    // Too large, or not a cache file (two processes that created it at
    // once): it starts over.
    let mut start = [0u8; 8];
    let usable = fs::File::open(&state.path)
        .and_then(|mut f| std::io::Read::read_exact(&mut f, &mut start))
        .is_ok_and(|()| &start == MAGIC)
        && fs::metadata(&state.path).is_ok_and(|m| m.len() <= MAX_FILE);
    if !usable {
        let _ = fs::remove_file(&state.path);
        state.tail = None;
    }
    // A new file starts with the magic (`create_new` succeeds only once).
    // The cache holds host code: the user's only.
    let _ = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&state.path)
        .and_then(|mut f| f.write_all(MAGIC));
    let out = core::mem::take(&mut state.new);
    let _ = fs::OpenOptions::new()
        .append(true)
        .open(&state.path)
        .and_then(|mut f| {
            // A record cut short at the end would hide the ones after it.
            if let Some(end) = state.tail.take() {
                f.set_len(end as u64)?;
            }
            f.write_all(&out)
        });
    if let Some(dir) = state.path.parent() {
        trim(dir, MAX_DIR);
    }
    // The records just written are in the file now, not to be written
    // again (nor found by a later lookup of this process).
    for places in state.index.values_mut() {
        places.keep_file_records();
    }
}

/// Removes the oldest cache files of `dir` (`*.jit`, by modification time)
/// until they take at most `limit` bytes together.
fn trim(dir: &std::path::Path, limit: u64) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut files: Vec<(std::time::SystemTime, u64, PathBuf)> = entries
        .filter_map(Result::ok)
        .filter(|e| e.path().extension().is_some_and(|x| x == "jit"))
        .filter_map(|e| {
            let m = e.metadata().ok()?;
            Some((m.modified().ok()?, m.len(), e.path()))
        })
        .collect();
    let mut total: u64 = files.iter().map(|f| f.1).sum();
    files.sort();
    for (_, len, path) in files {
        if total <= limit {
            break;
        }
        if fs::remove_file(&path).is_ok() {
            total -= len;
        }
    }
}

/// A record: the body's length (u32), its checksum (u64, `checksum`), then
/// the body: rip, key (u64), the instruction count, the number of guest
/// bytes, of words and of faults (u32 each), the guest bytes, the words,
/// the faults (pairs of u32). Little-endian.
fn encode(
    out: &mut Vec<u8>,
    rip: u64,
    key: u64,
    instructions: u32,
    bytes: &[u8],
    words: &[u32],
    faults: &[(u32, u32)],
) {
    let len = FIXED + bytes.len() + 4 * words.len() + 8 * faults.len();
    let start = out.len();
    out.reserve(HEADER + len);
    out.extend_from_slice(&(len as u32).to_le_bytes());
    out.extend_from_slice(&[0; 8]);
    out.extend_from_slice(&rip.to_le_bytes());
    out.extend_from_slice(&key.to_le_bytes());
    out.extend_from_slice(&instructions.to_le_bytes());
    out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    out.extend_from_slice(&(words.len() as u32).to_le_bytes());
    out.extend_from_slice(&(faults.len() as u32).to_le_bytes());
    out.extend_from_slice(bytes);
    for word in words {
        out.extend_from_slice(&word.to_le_bytes());
    }
    for &(at, slow) in faults {
        out.extend_from_slice(&at.to_le_bytes());
        out.extend_from_slice(&slow.to_le_bytes());
    }
    let sum = checksum(&out[start + HEADER..]);
    out[start + 4..start + 12].copy_from_slice(&sum.to_le_bytes());
}

fn u32_at(d: &[u8], i: usize) -> u32 {
    u32::from_le_bytes(d[i..i + 4].try_into().expect("4 bytes"))
}

fn u64_at(d: &[u8], i: usize) -> u64 {
    u64::from_le_bytes(d[i..i + 8].try_into().expect("8 bytes"))
}

/// The records of a file by their headers alone: (rip, key, offset), and
/// where the last one ends. Stops at a length that does not fit, or at one
/// whose counts do not add up.
fn scan(data: &[u8]) -> (Vec<(u64, u64, usize)>, usize) {
    let mut found = Vec::new();
    if data.len() < MAGIC.len() || &data[..MAGIC.len()] != MAGIC {
        return (found, 0);
    }
    let mut at = MAGIC.len();
    while data.len() - at >= HEADER + FIXED {
        let len = u32_at(data, at) as usize;
        let Some(body) = data.get(at + HEADER..at + HEADER + len) else {
            break;
        };
        if len < FIXED {
            break;
        }
        let (nb, nw, nf) = (
            u32_at(body, 20) as usize,
            u32_at(body, 24) as usize,
            u32_at(body, 28) as usize,
        );
        if FIXED + nb + 4 * nw + 8 * nf != len {
            break;
        }
        found.push((u64_at(body, 0), u64_at(body, 8), at));
        at += HEADER + len;
    }
    (found, at)
}

/// The record at `at` (its header and body), as `scan` found it.
fn record_at(data: &[u8], at: usize) -> &[u8] {
    let len = u32_at(data, at) as usize;
    &data[at..at + HEADER + len]
}

/// Whether the record's body matches its checksum.
fn intact(record: &[u8]) -> bool {
    checksum(&record[HEADER..]) == u64_at(record, 4)
}

fn guest_bytes(body: &[u8]) -> &[u8] {
    let nb = u32_at(body, 20) as usize;
    &body[FIXED..FIXED + nb]
}

/// A checksum of `data`, 32 bytes at a time in four independent lanes
/// (one chain of multiplications was a fifth of a warm Python startup's
/// lookups); accidental damage, not an adversary: ADR-0048.
fn checksum(data: &[u8]) -> u64 {
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mix = |h: u64, w: u64| (h ^ w).wrapping_mul(PRIME).rotate_left(31);
    let mut lanes = [
        0xcbf2_9ce4_8422_2325 ^ data.len() as u64,
        0x9e37_79b9_7f4a_7c15,
        0xc2b2_ae3d_27d4_eb4f,
        0x1656_67b1_9e37_79f9,
    ];
    let (blocks, rest) = data.as_chunks::<32>();
    for block in blocks {
        let (words, _) = block.as_chunks::<8>();
        for (lane, word) in lanes.iter_mut().zip(words) {
            *lane = mix(*lane, u64::from_le_bytes(*word));
        }
    }
    let mut h = lanes.iter().fold(0, |h, &lane| mix(h, lane));
    let (words, bytes) = rest.as_chunks::<8>();
    for word in words {
        h = mix(h, u64::from_le_bytes(*word));
    }
    for &b in bytes {
        h = (h ^ u64::from(b)).wrapping_mul(PRIME);
    }
    h
}

/// FNV-1a, 64 bits (cache file names and keys).
pub fn fnv1a(data: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in data {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// Tests: flips a bit of every record's first word (the budget's check,
/// which every run of a block executes), the checksums left as they were.
#[cfg(test)]
pub(crate) fn damage_first_words(data: &mut [u8]) {
    let (records, _) = scan(data);
    for (_, _, at) in records {
        let body = at + HEADER;
        let nb = u32_at(data, body + 20) as usize;
        data[body + FIXED + nb] ^= 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_round_trip_and_damage_is_found() {
        let mut data = MAGIC.to_vec();
        encode(
            &mut data,
            0x40_1000,
            7,
            3,
            &[0x90, 0xc3],
            &[1, 2, 3],
            &[(1, 2)],
        );
        encode(&mut data, 0x40_2000, 7, 1, &[0xc3], &[4], &[]);
        let (found, end) = scan(&data);
        assert_eq!((found.len(), end), (2, data.len()));
        assert_eq!((found[0].0, found[0].1), (0x40_1000, 7));
        let record = record_at(&data, found[0].2);
        assert!(intact(record));
        let body = &record[HEADER..];
        assert_eq!(guest_bytes(body), &[0x90, 0xc3]);
        let h = Hit { body };
        let words: Vec<u32> = h
            .words()
            .as_chunks::<4>()
            .0
            .iter()
            .map(|w| u32::from_le_bytes(*w))
            .collect();
        assert_eq!(
            (words, h.faults().collect::<Vec<_>>(), h.instructions()),
            (vec![1, 2, 3], vec![(1, 2)], 3)
        );
        // A flipped byte in the second record's words: found damaged.
        let mut damaged = data.clone();
        let last = damaged.len() - 1;
        damaged[last] ^= 1;
        let (found, _) = scan(&damaged);
        assert_eq!(found.len(), 2);
        assert!(!intact(record_at(&damaged, found[1].2)));
        // Cut short: the second is not found, and the first ends early.
        let (found, end) = scan(&data[..data.len() - 2]);
        assert_eq!(found.len(), 1);
        assert_eq!(end, found[0].2 + record_at(&data, found[0].2).len());
        // Not a cache file.
        assert!(scan(b"ARMORJC0").0.is_empty());
    }

    #[test]
    fn the_oldest_files_go_first() {
        let dir = std::env::temp_dir().join(format!("armor-cache-trim-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        for (i, name) in ["a.jit", "b.jit", "c.jit"].iter().enumerate() {
            fs::write(dir.join(name), vec![0u8; 100]).unwrap();
            let time =
                std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1000 + i as u64);
            fs::File::options()
                .write(true)
                .open(dir.join(name))
                .unwrap()
                .set_modified(time)
                .unwrap();
        }
        fs::write(dir.join("other"), vec![0u8; 500]).unwrap();
        trim(&dir, 200);
        assert!(!dir.join("a.jit").exists());
        assert!(dir.join("b.jit").exists() && dir.join("c.jit").exists());
        assert!(dir.join("other").exists(), "only cache files are removed");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_few_records_per_address() {
        let mut index = Index::default();
        for i in 0..10 {
            push(&mut index, 1, 2, Place::New(i));
        }
        let kept: Vec<usize> = index[&(1, 2)]
            .as_slice()
            .iter()
            .map(|p| match p {
                Place::New(i) | Place::File(i) => *i,
            })
            .collect();
        // The newest, oldest first.
        assert_eq!(kept, (10 - PER_ADDRESS..10).collect::<Vec<_>>());
    }
}
