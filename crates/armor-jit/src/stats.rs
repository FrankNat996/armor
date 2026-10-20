//! Translation counters for the whole process, all threads together:
//! how much the JIT translated and how long it took, and which kinds of
//! instruction it left to the interpreter. armor prints them at exit when
//! `ARMOR_JIT_STATS` is set (`cargo xtask bench`,
//! docs/tech/performance.md); the interpreted kinds also every
//! `REPORT_EVERY` interpreted instructions, for programs that run long.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use armor_x86::{Instruction, Op, Operand, Reg};

static BLOCKS: AtomicU64 = AtomicU64::new(0);
static GUEST_INSTRUCTIONS: AtomicU64 = AtomicU64::new(0);
static HOST_INSTRUCTIONS: AtomicU64 = AtomicU64::new(0);
static NANOS: AtomicU64 = AtomicU64::new(0);

/// One translated block: its guest instructions, the AArch64 words
/// generated for it, and the time spent decoding and translating it.
pub(crate) fn record(guest: usize, host: usize, time: Duration) {
    BLOCKS.fetch_add(1, Ordering::Relaxed);
    GUEST_INSTRUCTIONS.fetch_add(guest as u64, Ordering::Relaxed);
    HOST_INSTRUCTIONS.fetch_add(host as u64, Ordering::Relaxed);
    NANOS.fetch_add(
        u64::try_from(time.as_nanos()).unwrap_or(u64::MAX),
        Ordering::Relaxed,
    );
}

/// The counters as one line, in a fixed format that `cargo xtask bench`
/// parses.
pub fn summary() -> String {
    format!(
        "jit stats: {} blocks, {} guest instructions, {} host instructions, {} us translating",
        BLOCKS.load(Ordering::Relaxed),
        GUEST_INSTRUCTIONS.load(Ordering::Relaxed),
        HOST_INSTRUCTIONS.load(Ordering::Relaxed),
        NANOS.load(Ordering::Relaxed) / 1000
    )
}

static INTERPRETED_ON: AtomicBool = AtomicBool::new(false);
static INTERPRETED_TOTAL: AtomicU64 = AtomicU64::new(0);
static INTERPRETED: OnceLock<Mutex<HashMap<String, u64>>> = OnceLock::new();

/// Interpreted instructions between two reports to stderr.
const REPORT_EVERY: u64 = 1 << 24;
/// The kinds a report lists.
const REPORT_KINDS: usize = 25;

/// Counts the instructions the engine interprets, by kind (slow: for
/// finding what to translate next).
pub fn count_interpreted() {
    INTERPRETED_ON.store(true, Ordering::Relaxed);
}

pub fn counting_interpreted() -> bool {
    INTERPRETED_ON.load(Ordering::Relaxed)
}

/// The kind of an instruction: the operation's name; for SSE the whole
/// SSE operation (its fields are small) and whether it is on MMX
/// registers; for x87 the inner operation's name. From the decoder's
/// `Debug` text.
fn kind(insn: &Instruction) -> String {
    if let Op::Sse { op, dst, src } = insn.op {
        let mmx = [dst, src]
            .iter()
            .any(|o| matches!(o, Operand::Reg(Reg::Mm(_))));
        return format!("Sse/{op:?}{}", if mmx { " (mmx)" } else { "" });
    }
    let text = format!("{:?}", insn.op);
    let name_end = |s: &str| s.find([' ', '(', '{', ',']).unwrap_or(s.len());
    let outer = &text[..name_end(&text)];
    match text.find("op: ") {
        Some(at) => {
            let inner = &text[at + 4..];
            format!("{outer}/{}", &inner[..name_end(inner)])
        }
        None => outer.to_string(),
    }
}

/// One interpreted instruction (`None`: it did not decode).
pub(crate) fn record_interpreted(insn: Option<&Instruction>) {
    let key = insn.map_or_else(|| "undecodable".to_string(), kind);
    let map = INTERPRETED.get_or_init(|| Mutex::new(HashMap::new()));
    *map.lock()
        .unwrap_or_else(|e| e.into_inner())
        .entry(key)
        .or_insert(0) += 1;
    if INTERPRETED_TOTAL.fetch_add(1, Ordering::Relaxed) % REPORT_EVERY == REPORT_EVERY - 1 {
        eprintln!("armor: {}", interpreted_summary());
    }
}

/// The most interpreted kinds, most first.
pub fn interpreted_summary() -> String {
    let Some(map) = INTERPRETED.get() else {
        return "jit interpreted: nothing".to_string();
    };
    let map = map.lock().unwrap_or_else(|e| e.into_inner());
    let mut kinds: Vec<_> = map.iter().collect();
    kinds.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
    let total: u64 = map.values().sum();
    let list: Vec<String> = kinds
        .iter()
        .take(REPORT_KINDS)
        .map(|(k, n)| format!("{k} {n}"))
        .collect();
    format!("jit interpreted: {total} instructions; {}", list.join(", "))
}

/// `/tmp/perf-<pid>.map` when `ARMOR_PERF_MAP` is set: one line per
/// translated block, its code and the guest address it came from, so that
/// `perf report` names the JIT's code by the x86 code it runs
/// (docs/tech/performance.md). Opened at the first block.
static PERF_MAP: std::sync::OnceLock<Option<std::sync::Mutex<std::fs::File>>> =
    std::sync::OnceLock::new();

fn perf_map_file() -> Option<&'static std::sync::Mutex<std::fs::File>> {
    PERF_MAP
        .get_or_init(|| {
            std::env::var_os("ARMOR_PERF_MAP")?;
            let path = format!("/tmp/perf-{}.map", std::process::id());
            std::fs::OpenOptions::new()
                .create(true)
                .read(true)
                .append(true)
                .open(path)
                .ok()
                .map(std::sync::Mutex::new)
        })
        .as_ref()
}

/// Drops the perf map's lines for code in `[start, start + len)`: a code
/// buffer was emptied, and its addresses will name other blocks (perf
/// would otherwise name a sample after whichever old block it finds).
pub fn perf_map_forget(start: *const u8, len: usize) {
    let Some(file) = perf_map_file() else {
        return;
    };
    use std::io::{Read, Seek, Write};
    let mut file = file.lock().expect("perf map lock");
    let mut text = String::new();
    if file
        .rewind()
        .and_then(|_| file.read_to_string(&mut text))
        .is_err()
    {
        return;
    }
    let range = start as usize..start as usize + len;
    let kept: String = text
        .lines()
        .filter(|line| {
            let address = line.split(' ').next().unwrap_or("");
            usize::from_str_radix(address, 16).map_or(true, |a| !range.contains(&a))
        })
        .flat_map(|line| [line, "\n"])
        .collect();
    // Append mode writes at the end, which is now the start.
    let _ = file
        .set_len(0)
        .and_then(|_| file.write_all(kept.as_bytes()));
}

/// Records a published block in the perf map, if there is one.
pub fn perf_map(code: *const u8, bytes: usize, rip: u64, instructions: usize) {
    if let Some(file) = perf_map_file() {
        use std::io::Write;
        let line = format!(
            "{:x} {bytes:x} x86 {rip:#x} ({instructions} instructions)\n",
            code as usize
        );
        let _ = file
            .lock()
            .expect("perf map lock")
            .write_all(line.as_bytes());
    }
}
