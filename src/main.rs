// nyi — a good fetch.

mod cache;
mod logos;
mod mobile;
mod panel;

use logos::{builtin_logo, default_logo};

use std::ffi::{CStr, CString};
use std::fs;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};

const VERSION_RAW: &str = include_str!("../VERSION");
const CODENAME: &str = match option_env!("NYI_CODENAME") {
    Some(s) => s,
    None => "nyiotxa",
};
const BUILD_ARCH: &str = std::env::consts::ARCH;
#[cfg(target_os = "macos")]
const BUILD_OS: &str = "Darwin";
#[cfg(all(not(target_os = "macos"), target_os = "android"))]
const BUILD_OS: &str = "Android";
#[cfg(all(not(target_os = "macos"), not(target_os = "android")))]
const BUILD_OS: &str = "Linux";

// geometry
const ANIM_WIDTH: i32 = 60;
const MAX_HEIGHT: i32 = 200;
const GAP: usize = 2;
const MAX_SUB_ROWS: usize = 3;
const MAX_SUB_COLS: usize = 2;
const SUB_W: usize = ANIM_WIDTH as usize * MAX_SUB_COLS;
const SUB_H: usize = MAX_HEIGHT as usize * MAX_SUB_ROWS;

// ramps for shading
const RAMP_ASCII: &str = ".,-~:;=!*#$@";
const RAMP_BLOCKS: &str = "░▒▓█";

const MAX_LOGO_ROWS: usize = 64;
const MAX_LOGO_COLS: usize = 128;
const MAX_INFO_LINES: usize = 32;
const MAX_EXTRA_DISKS: usize = 8;
const MAX_SHADING: usize = 64;
const MAX_POINTS: usize = 200_000;
const GIB: f64 = 1024.0 * 1024.0 * 1024.0;

// default colors
const DEFAULT_COLOR_INNER: &str = "\x1b[1;37m";
const DEFAULT_COLOR_OUTER: &str = "\x1b[1;35m";

fn version() -> &'static str {
    VERSION_RAW.trim()
}

// terminal Stage

static TERM_RESIZED: AtomicBool = AtomicBool::new(false);
static TERMIOS_SAVED: AtomicBool = AtomicBool::new(false);
/// stdin's original fcntl flags; -1 means we never touched them.
static ORIG_STDIN_FLAGS: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(-1);
static mut ORIG_TERMIOS: std::mem::MaybeUninit<libc::termios> = std::mem::MaybeUninit::uninit();

// writing an full buffer to a file descriptor
fn write_fd(fd: i32, buf: &[u8]) -> bool {
    let mut off = 0usize;
    while off < buf.len() {
        let n = unsafe {
            libc::write(
                fd,
                buf[off..].as_ptr() as *const libc::c_void,
                buf.len() - off,
            )
        };
        if n <= 0 {
            let err = std::io::Error::last_os_error();
            match err.raw_os_error() {
                Some(libc::EINTR) => continue,
                // stdin's O_NONBLOCK reaches stdout too when they share a
                // tty; a slow terminal is a reason to wait, not to quit
                Some(e) if e == libc::EAGAIN || e == libc::EWOULDBLOCK => {
                    sleep_ms(1);
                    continue;
                }
                _ => return false,
            }
        }
        off += n as usize;
    }
    true
}

fn out(bytes: &[u8]) {
    write_fd(libc::STDOUT_FILENO, bytes);
}

/// Async signal cleanup: restore the original terminal mode and show the cursor.
fn cleanup() {
    unsafe {
        if TERMIOS_SAVED.load(Ordering::Relaxed) {
            let p = std::ptr::addr_of!(ORIG_TERMIOS) as *const libc::termios;
            libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, p);
        }
        let flags = ORIG_STDIN_FLAGS.load(Ordering::Relaxed);
        if flags != -1 {
            libc::fcntl(libc::STDIN_FILENO, libc::F_SETFL, flags);
        }
    }
    // end a synchronized update a signal may have cut short, drop any color
    // left on, and give the cursor back
    out(b"\x1b[0m\x1b[?2026l\x1b[?25h");
}

extern "C" fn handle_signal(_sig: libc::c_int) {
    cleanup();
    unsafe { libc::_exit(0) };
}

extern "C" fn handle_winch(_sig: libc::c_int) {
    TERM_RESIZED.store(true, Ordering::Relaxed);
}

/// Rust's runtime ignores SIGPIPE so a closed stdout comes back as an io
/// error, which for us means `nyi --dump-logo apple | head` panics instead of
/// quietly stopping. Put the default back before anything prints.
fn restore_sigpipe() {
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
}

fn install_signal_handlers() {
    unsafe {
        libc::signal(
            libc::SIGINT,
            handle_signal as *const () as libc::sighandler_t,
        );
        libc::signal(
            libc::SIGTERM,
            handle_signal as *const () as libc::sighandler_t,
        );
        libc::signal(
            libc::SIGWINCH,
            handle_winch as *const () as libc::sighandler_t,
        );
    }
}

/// witch stdin to raw-ish mode so a keypress ends the animation
fn enter_raw_mode() {
    unsafe {
        let p = std::ptr::addr_of_mut!(ORIG_TERMIOS) as *mut libc::termios;
        if libc::tcgetattr(libc::STDIN_FILENO, p) != 0 {
            return;
        }
        TERMIOS_SAVED.store(true, Ordering::Relaxed);
        let mut raw = std::ptr::read(p);
        raw.c_lflag &= !(libc::ICANON | libc::ECHO);
        raw.c_cc[libc::VMIN] = 0;
        raw.c_cc[libc::VTIME] = 0;
        libc::tcsetattr(libc::STDIN_FILENO, libc::TCSAFLUSH, &raw);

        // VMIN=0/VTIME=0 should be enough to keep read() from waiting, but not
        // every pty honours it (Termux's doesn't), and one blocking read locks
        // the program up before a single frame is drawn. O_NONBLOCK guarantees
        // it regardless of the terminal settings.
        let flags = libc::fcntl(libc::STDIN_FILENO, libc::F_GETFL);
        if flags != -1
            && libc::fcntl(libc::STDIN_FILENO, libc::F_SETFL, flags | libc::O_NONBLOCK) != -1
        {
            ORIG_STDIN_FLAGS.store(flags, Ordering::Relaxed);
        }

        // TCSAFLUSH drains the buffer, but on some terminals the Enter that
        // started the command lands after tcsetattr. Drop what's left, or the
        // first key check sees it and exits immediately.
        let mut sink = [0u8; 64];
        for _ in 0..16 {
            let n = libc::read(
                libc::STDIN_FILENO,
                sink.as_mut_ptr() as *mut libc::c_void,
                sink.len(),
            );
            if n <= 0 {
                break;
            }
        }
    }
}

// libc doesn't re-export TIOCGWINSZ for Darwin, so spell it out there:
// _IOR('t', 104, struct winsize).
#[cfg(target_os = "macos")]
const TIOCGWINSZ: libc::c_ulong = 0x4008_7468;
#[cfg(not(target_os = "macos"))]
const TIOCGWINSZ: libc::c_ulong = libc::TIOCGWINSZ as libc::c_ulong;

/// (rows, cols) of the terminal, 0 for whatever we couldn't learn.
fn get_term_size() -> (i32, i32) {
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    let ok = unsafe { libc::ioctl(libc::STDOUT_FILENO, TIOCGWINSZ as _, &mut ws) } == 0;
    if !ok {
        return (0, 0);
    }
    (
        if ws.ws_row > 0 { ws.ws_row as i32 } else { 0 },
        if ws.ws_col > 0 { ws.ws_col as i32 } else { 0 },
    )
}

/// true if there is input waiting on stdin.
///
/// enter_raw_mode() sets VMIN=0/VTIME=0, so read() returns without waiting and
/// poll() isn't needed. That matters: environments with an incomplete syscall
/// table, iSH among them, kill the process with SIGSYS on poll().
fn key_pressed() -> bool {
    // Without raw mode (stdin isn't a tty) read() would block.
    if !TERMIOS_SAVED.load(Ordering::Relaxed) || ORIG_STDIN_FLAGS.load(Ordering::Relaxed) == -1 {
        return false;
    }
    let mut b = [0u8; 1];
    unsafe { libc::read(libc::STDIN_FILENO, b.as_mut_ptr() as *mut libc::c_void, 1) > 0 }
}

/// std::thread::sleep uses clock_nanosleep on Linux, which iSH doesn't
/// implement. nanosleep is far older and available everywhere.
fn sleep_ms(ms: i64) {
    let ts = libc::timespec {
        tv_sec: (ms / 1000) as libc::time_t,
        tv_nsec: ((ms % 1000) * 1_000_000) as _,
    };
    unsafe { libc::nanosleep(&ts, std::ptr::null_mut()) };
}

// sys-helpers
struct Uname {
    sysname: String,
    release: String,
    version: String,
    machine: String,
}

fn uname() -> Uname {
    unsafe {
        let mut u: libc::utsname = std::mem::zeroed();
        if libc::uname(&mut u) != 0 {
            return Uname {
                sysname: String::new(),
                release: String::new(),
                version: String::new(),
                machine: String::new(),
            };
        }
        let f = |b: &[libc::c_char]| CStr::from_ptr(b.as_ptr()).to_string_lossy().into_owned();
        Uname {
            sysname: f(&u.sysname),
            release: f(&u.release),
            version: f(&u.version),
            machine: f(&u.machine),
        }
    }
}

fn hostname() -> String {
    let mut buf = [0 as libc::c_char; 64];
    unsafe {
        if libc::gethostname(buf.as_mut_ptr(), buf.len()) != 0 {
            return String::new();
        }
        buf[buf.len() - 1] = 0;
        CStr::from_ptr(buf.as_ptr()).to_string_lossy().into_owned()
    }
}

fn login_name() -> String {
    unsafe {
        let p = libc::getlogin();
        if !p.is_null() {
            let s = CStr::from_ptr(p).to_string_lossy().into_owned();
            if !s.is_empty() {
                return s;
            }
        }
    }
    std::env::var("USER").unwrap_or_default()
}

fn home_dir() -> Option<String> {
    std::env::var("HOME").ok().filter(|h| !h.is_empty())
}

/// $XDG_CONFIG_HOME
fn xdg_config_home() -> Option<String> {
    if let Ok(x) = std::env::var("XDG_CONFIG_HOME") {
        if !x.is_empty() {
            return Some(x);
        }
    }
    home_dir().map(|h| format!("{h}/.config"))
}

/// base directory for nyi's own files.
fn config_dir() -> Option<String> {
    xdg_config_home().map(|c| format!("{c}/nyi"))
}

/// where to look for nyi's config files.
fn config_search_paths(name: &str) -> Vec<String> {
    let mut paths = Vec::new();
    if let Some(d) = config_dir() {
        paths.push(format!("{d}/{name}"));
    }
    let dirs = std::env::var("XDG_CONFIG_DIRS").unwrap_or_default();
    let dirs = if dirs.is_empty() {
        "/etc/xdg".to_string()
    } else {
        dirs
    };
    for d in dirs.split(':').filter(|d| !d.is_empty()) {
        paths.push(format!("{d}/nyi/{name}"));
    }
    paths
}

/// first readable path from the search list.
fn find_config_file(name: &str) -> Option<String> {
    config_search_paths(name)
        .into_iter()
        .find(|p| fs::metadata(p).is_ok())
}

/// whole file as lossy UTF-8, or none if it can't be read.
fn read_file(path: &str) -> Option<String> {
    fs::read(path)
        .ok()
        .map(|b| String::from_utf8_lossy(&b).into_owned())
}
#[cfg(not(target_os = "macos"))]
fn read_first_line(path: &str) -> Option<String> {
    let s = read_file(path)?;
    let line = s.split('\n').next().unwrap_or("");
    let t = line.trim_end_matches(['\r', '\0']).to_string();
    if t.is_empty() {
        None
    } else {
        Some(t)
    }
}

/// first whitespace-separated integer in a file.
#[cfg(not(target_os = "macos"))]
fn read_i64(path: &str) -> Option<i64> {
    let s = read_file(path)?;
    s.split_ascii_whitespace().next()?.parse::<i64>().ok()
}

/// run a shell command and capture its stdout.
fn run(cmd: &str) -> Option<String> {
    run_argv("sh", &["-c", cmd])
}

/// run a program directly, no shell in between, and capture its stdout. Each
/// `sh -c` costs a whole extra process, and these add up at startup.
fn run_argv(bin: &str, args: &[&str]) -> Option<String> {
    let o = Command::new(bin)
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    Some(String::from_utf8_lossy(&o.stdout).into_owned())
}

fn is_executable(path: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// Where `name` resolves on PATH, without spawning anything. Names with a
/// slash are taken as paths.
fn which(name: &str) -> Option<String> {
    if name.is_empty() {
        return None;
    }
    if name.contains('/') {
        return is_executable(name).then(|| name.to_string());
    }
    let path = std::env::var("PATH").unwrap_or_default();
    path.split(':')
        .filter(|d| !d.is_empty())
        .map(|d| format!("{d}/{name}"))
        .find(|p| is_executable(p))
}

/// only this charset is allowed anywhere we interpolate into a shell command.
fn is_shell_safe(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'/' | b'_' | b'-' | b'+'))
}

/// a PCI_ID from a uevent.
#[cfg(not(target_os = "macos"))]
fn is_pci_id(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|c| c.is_ascii_hexdigit() || c == b':')
}

/// runs <bin> --version and returns the version number in its first line.
///
/// Remembered across runs, keyed by the resolved binary and its size and mtime,
/// so an upgrade (or a new store path) is a miss. A binary that isn't on PATH
/// answers "" without a process ever being started.
fn cmd_version(bin: &str) -> String {
    if !is_shell_safe(bin) {
        return String::new();
    }
    let Some(path) = which(bin) else {
        return String::new();
    };
    let real = fs::canonicalize(&path)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| path.clone());
    let stamp = {
        use std::os::unix::fs::MetadataExt;
        fs::metadata(&real)
            .map(|m| format!("{}:{}", m.mtime(), m.size()))
            .unwrap_or_default()
    };
    // the invoked path is part of the key: busybox and toybox answer for a
    // whole set of names from one binary
    cache::get_or(&format!("ver:{path}=>{real}:{stamp}"), || {
        let Some(o) = run_argv(&path, &["--version"]) else {
            return String::new();
        };
        let first = o.split('\n').next().unwrap_or("");
        let Some(start) = first.find(|c: char| c.is_ascii_digit()) else {
            return String::new();
        };
        first[start..]
            .chars()
            .take_while(|&c| c != ' ' && c != '(' && c != '\n')
            .take(30)
            .collect()
    })
}

fn eq_ci(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

#[cfg(not(target_os = "macos"))]
fn contains_ci(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() || needle.len() > haystack.len() {
        return false;
    }
    haystack
        .to_ascii_lowercase()
        .contains(&needle.to_ascii_lowercase())
}

/// percentage of colors
fn pct_color(pct: i64) -> &'static str {
    if pct >= 80 {
        "31"
    } else if pct >= 50 {
        "93"
    } else {
        "32"
    }
}

// UTF-8 helpers
/// byte length of a UTF-8 sequence from its leading byte.
fn utf8_char_len(c: u8) -> usize {
    if c < 0x80 {
        1
    } else if c & 0xE0 == 0xC0 {
        2
    } else if c & 0xF0 == 0xE0 {
        3
    } else if c & 0xF8 == 0xF0 {
        4
    } else {
        1 // invalid, treat as 1
    }
}

/// Bytes to skip past an ANSI escape sequence.
fn skip_ansi(p: &[u8]) -> usize {
    if p.len() < 2 || p[0] != 0x1b || p[1] != b'[' {
        return 0;
    }
    let mut i = 2;
    while i < p.len() && (p[i].is_ascii_digit() || p[i] == b';') {
        i += 1;
    }
    if i < p.len() {
        i += 1;
    }
    i
}
fn is_cursor_escape(p: &[u8]) -> bool {
    if p.len() < 2 || p[0] != 0x1b || p[1] != b'[' {
        return false;
    }
    let mut i = 2;
    while i < p.len() && (p[i].is_ascii_digit() || p[i] == b';') {
        i += 1;
    }
    i < p.len() && p[i] != b'm'
}

/// visible columns of a string.
fn visible_width(s: &str) -> usize {
    let b = s.as_bytes();
    let mut i = 0;
    let mut w = 0;
    while i < b.len() {
        let a = skip_ansi(&b[i..]);
        if a > 0 {
            i += a;
            continue;
        }
        i += utf8_char_len(b[i]).min(b.len() - i);
        w += 1;
    }
    w
}

/// Clip a string to a maximum visible width.
fn emit_clipped(out: &mut Vec<u8>, s: &str, max_cols: i32) {
    let b = s.as_bytes();

    // first pass: measure visible width to know if we need to clip
    let total_w = visible_width(s) as i32;
    let need_clip = max_cols >= 0 && total_w > max_cols;
    let limit = if need_clip && max_cols >= 6 {
        max_cols - 3 // leave room for "..."
    } else {
        max_cols
    };

    let mut i = 0;
    let mut w = 0i32;
    let mut had_ansi = false;
    while i < b.len() {
        let a = skip_ansi(&b[i..]);
        if a > 0 {
            out.extend_from_slice(&b[i..i + a]);
            i += a;
            had_ansi = true;
            continue;
        }
        if limit >= 0 && w >= limit {
            break;
        }
        let n = utf8_char_len(b[i]).min(b.len() - i);
        out.extend_from_slice(&b[i..i + n]);
        i += n;
        w += 1;
    }
    if need_clip && max_cols >= 6 {
        out.extend_from_slice(b"...");
    }
    if had_ansi && need_clip {
        out.extend_from_slice(b"\x1b[0m");
    }
}

/// strip a trailing " (<hint>)" from a string, if present.
fn strip_inline_hint(val: &str) -> &str {
    let cut = match val.find(" (") {
        Some(i) => &val[..i],
        None => val,
    };
    cut.trim_end_matches([' ', '\t'])
}

// shading
// quadrants by coverage mask.
const QUADRANT_GLYPHS: [&str; 16] = [
    " ", "▘", "▝", "▀", "▖", "▌", "▞", "▛", "▗", "▚", "▐", "▜", "▄", "▙", "▟", "█",
];

struct Shading {
    /// one entry per codepoint dimmest first.
    chars: Vec<String>,
    sub_rows: usize,
    sub_cols: usize,
    /// Sextants by coverage mask
    sextants: Vec<String>,
}

impl Shading {
    fn smax(&self) -> usize {
        self.chars.len() - 1
    }
}

/// build the sextant glyphs, skipping the two that are already in QUADRANT_GLYPHS.
fn build_sextant_glyphs() -> Vec<String> {
    let mut g = vec![String::new(); 64];
    for (mask, glyph) in g.iter_mut().enumerate().take(63).skip(1) {
        if mask == 21 || mask == 42 {
            *glyph = if mask == 21 { "▌" } else { "▐" }.to_string();
            continue;
        }
        let cp = 0x1FB00u32 + mask as u32 - 1 - (mask > 21) as u32 - (mask > 42) as u32;
        if let Some(c) = char::from_u32(cp) {
            *glyph = c.to_string();
        }
    }
    // cell_glyph never asks for 0 or 63 today, but leaving them empty is a trap
    g[63] = "█".to_string();
    g
}

/// parse a UTF-8 string into individual codepoints.
fn parse_shading(s: &str) -> Vec<String> {
    let v: Vec<String> = s.chars().take(MAX_SHADING).map(|c| c.to_string()).collect();
    if v.is_empty() {
        vec![".".to_string()] // fallback
    } else {
        v
    }
}

/// mode is ascii/blocks/sextants, or None for the ascii default.
fn select_shading(mode: Option<&str>, chars: Option<&str>) -> Option<Shading> {
    let mode = mode.unwrap_or("ascii");
    let (sub_rows, sub_cols, sextants) = match mode {
        "sextants" => (3, 2, build_sextant_glyphs()),
        "blocks" => (2, 2, Vec::new()),
        "ascii" => (1, 1, Vec::new()),
        _ => return None,
    };
    let ramp = match chars {
        Some(c) => c,
        None if mode == "ascii" => RAMP_ASCII,
        None => RAMP_BLOCKS,
    };
    Some(Shading {
        chars: parse_shading(ramp),
        sub_rows,
        sub_cols,
        sextants,
    })
}

// logo
struct Logo {
    cells: Vec<Vec<String>>,
    colors: Vec<Vec<i32>>,
    rows: usize,
    cols: usize,
    has_ansi: bool,
}

impl Logo {
    // split each row into codepoints and track the color of each codepoint.
    fn process(raw: &[String]) -> Logo {
        let mut logo = Logo {
            cells: Vec::new(),
            colors: Vec::new(),
            rows: raw.len().min(MAX_LOGO_ROWS),
            cols: 0,
            has_ansi: false,
        };
        for row in raw.iter().take(MAX_LOGO_ROWS) {
            let (cells, colors, saw_color) = Self::process_row(row);
            logo.cols = logo.cols.max(cells.len());
            logo.has_ansi |= saw_color;
            logo.cells.push(cells);
            logo.colors.push(colors);
        }
        logo
    }

    fn process_row(row: &str) -> (Vec<String>, Vec<i32>, bool) {
        let b = row.as_bytes();
        let mut cells = Vec::new();
        let mut colors = Vec::new();
        let mut cur_color = 0i32;
        let mut has_ansi = false;
        let mut i = 0usize;

        while i < b.len() && cells.len() < MAX_LOGO_COLS {
            if b[i] == 0x1b && i + 1 < b.len() && b[i + 1] == b'[' {
                let mut j = i + 2;
                let mut num = 0i32;
                let mut has_num = false;
                while j < b.len() && (b[j].is_ascii_digit() || b[j] == b';') {
                    if b[j].is_ascii_digit() {
                        num = num * 10 + (b[j] - b'0') as i32;
                        has_num = true;
                    } else {
                        if has_num
                            && ((30..=37).contains(&num) || num == 39 || (90..=97).contains(&num))
                        {
                            cur_color = num;
                        }
                        if has_num && (num == 0 || num == 22) {
                            cur_color = 0;
                        }
                        num = 0;
                        has_num = false;
                    }
                    j += 1;
                }
                if has_num && ((30..=37).contains(&num) || num == 39 || (90..=97).contains(&num)) {
                    cur_color = num;
                }
                if has_num && num == 0 {
                    cur_color = 0;
                }
                if j < b.len() {
                    j += 1;
                }
                if cur_color > 0 {
                    has_ansi = true;
                }
                i = j;
                continue;
            }
            let n = utf8_char_len(b[i]).min(b.len() - i);
            cells.push(String::from_utf8_lossy(&b[i..i + n]).into_owned());
            colors.push(cur_color);
            i += n;
        }
        (cells, colors, has_ansi)
    }

    fn weight_at(&self, row: usize, col: usize) -> f32 {
        match self.cells.get(row).and_then(|r| r.get(col)) {
            Some(c) => char_weight(c),
            None => 0.0,
        }
    }

    fn color_at(&self, row: usize, col: usize) -> i32 {
        self.colors
            .get(row)
            .and_then(|r| r.get(col))
            .copied()
            .unwrap_or(0)
    }
}

/// visual density of a single codepoint
fn char_weight(cell: &str) -> f32 {
    let b = cell.as_bytes();
    if b.is_empty() {
        return 0.0;
    }
    // single-byte ASCII
    if b[0] < 0x80 {
        return match b[0] {
            b'M' => 1.00,
            b'N' => 0.88,
            b'm' => 0.76,
            b'd' => 0.66,
            b'h' => 0.56,
            b'b' => 0.56,
            b'y' => 0.46,
            b'o' => 0.38,
            b'n' => 0.38,
            b's' => 0.30,
            b'+' => 0.22,
            b':' => 0.18,
            b'=' => 0.22,
            b'-' => 0.14,
            b'`' => 0.08,
            b'.' => 0.10,
            b'/' => 0.12,
            b'\'' => 0.06,
            b' ' => 0.0,
            c if c.is_ascii_uppercase() => 0.80,
            c if c.is_ascii_lowercase() => 0.50,
            c if c.is_ascii_digit() => 0.40,
            _ => 0.15,
        };
    }
    match cell.chars().next() {
        Some('█') => 1.00,
        Some('▓') => 0.75,
        Some('▒') => 0.50,
        Some('░') => 0.25,
        Some('▀') | Some('▄') | Some('▌') | Some('▐') => 0.50,
        Some('▛') | Some('▜') | Some('▙') | Some('▟') => 0.75,
        Some('▖') | Some('▗') | Some('▘') | Some('▝') => 0.25,
        Some(c) if ('\u{2500}'..='\u{257F}').contains(&c) => 0.20,
        Some(c) if ('\u{2800}'..='\u{28FF}').contains(&c) => {
            (0x80u32 | (c as u32 & 0x3F)).count_ones() as f32 / 8.0
        }
        _ => 0.30,
    }
}

/// trim trailing empty rows and limit to MAX_LOGO_ROWS.
fn trim_logo_rows(mut rows: Vec<String>) -> Vec<String> {
    rows.truncate(MAX_LOGO_ROWS);
    while rows.last().is_some_and(|r| r.is_empty()) {
        rows.pop();
    }
    rows
}

fn strip_eol(s: &str) -> &str {
    s.trim_end_matches(['\n', '\r'])
}

/// ~/.config/nyi/logo.txt, plus the "# distro: <name>" hint if present.
fn load_logo_file() -> Option<(Vec<String>, Option<String>)> {
    let text = read_file(&find_config_file("logo.txt")?)?;
    let mut rows: Vec<String> = Vec::new();
    let mut distro = None;
    for line in text.split('\n') {
        let line = strip_eol(line);
        if rows.is_empty() {
            if let Some(v) = line.strip_prefix("# distro:") {
                let v = v.trim_start_matches(' ');
                if !v.is_empty() {
                    distro = Some(v.chars().take(63).collect::<String>());
                }
                continue;
            }
        }
        if line.is_empty() && rows.is_empty() {
            continue;
        }
        if rows.len() >= MAX_LOGO_ROWS {
            break;
        }
        rows.push(line.to_string());
    }
    let rows = trim_logo_rows(rows);
    if rows.is_empty() {
        None
    } else {
        Some((rows, distro))
    }
}

/// cutting fastfetch logo on first cursor movement escape, but keep the last SGR (color reset) if present.
fn ff_truncate_line(buf: &mut Vec<u8>) -> bool {
    let mut last_sgr_end: i32 = -1;
    let mut i = 0usize;
    while i + 2 < buf.len() {
        if is_cursor_escape(&buf[i..]) {
            // if we found a previous complete SGR, cut after it (keep the reset)
            let cut = if last_sgr_end >= 0 {
                last_sgr_end as usize
            } else {
                i
            };
            buf.truncate(cut);
            return true;
        }
        // track end positions of SGR sequences
        if buf[i] == 0x1b && buf[i + 1] == b'[' {
            let mut j = i + 2;
            while j < buf.len() && (buf[j].is_ascii_digit() || buf[j] == b';') {
                j += 1;
            }
            if j < buf.len() && buf[j] == b'm' {
                last_sgr_end = (j + 1) as i32;
                i = j;
            }
        }
        i += 1;
    }
    false
}

/// load logo from fastfetch
///
/// --logo-type builtin is not optional: a user config that sets `logo.type` to
/// `file` makes fastfetch read `-l <name>` as a path, and when that path isn't
/// there it quietly draws the local machine's logo instead of the one we asked
/// for.
fn load_logo_ff_colored(name: &str) -> Option<Vec<String>> {
    if !is_shell_safe(name) || !have_bin("fastfetch") {
        return None;
    }
    let text = run_argv(
        "fastfetch",
        &[
            "-l",
            name,
            "--logo-type",
            "builtin",
            "-s",
            "break",
            "--pipe",
            "false",
        ],
    )?;
    let mut rows: Vec<String> = Vec::new();
    for line in text.split('\n') {
        if rows.len() >= MAX_LOGO_ROWS {
            break;
        }
        let mut buf = strip_eol(line).as_bytes().to_vec();
        let truncated = ff_truncate_line(&mut buf);
        if buf.is_empty() && rows.is_empty() {
            continue;
        }
        if buf.is_empty() && truncated {
            break;
        }
        rows.push(String::from_utf8_lossy(&buf).into_owned());
    }
    let rows = trim_logo_rows(rows);
    if rows.is_empty() {
        None
    } else {
        Some(rows)
    }
}

/// true if fastfetch lists a logo under exactly this name.
///
/// Asked for a name it doesn't have, fastfetch draws the local machine's logo
/// and exits 0, so there is no way to tell a hit from a miss after the fact.
/// The listing is the only honest answer.
fn ff_knows_logo(name: &str) -> bool {
    let Some(text) = ff_print_logos() else {
        return false;
    };
    text.split('\n').any(|line| {
        let line = strip_eol(line);
        line.len() == name.len() + 1 && line.ends_with(':') && eq_ci(&line[..line.len() - 1], name)
    })
}

/// fallback to fastfetch --print-logos if the colored output didn't work.
/// `fastfetch --print-logos`, run at most once.
fn ff_print_logos() -> Option<&'static str> {
    static CACHE: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    CACHE
        .get_or_init(|| {
            have_bin("fastfetch")
                .then(|| run_argv("fastfetch", &["--print-logos"]))
                .flatten()
        })
        .as_deref()
}

fn load_logo_ff_plain(name: &str) -> Option<Vec<String>> {
    let text = ff_print_logos()?;
    let mut rows: Vec<String> = Vec::new();
    let mut found = false;

    for line in text.split('\n') {
        let line = strip_eol(line);
        let len = line.len();

        if !found {
            if len > 0
                && len <= name.len() + 1
                && line.ends_with(':')
                && eq_ci(&line[..len - 1], name)
            {
                found = true;
            }
            continue;
        }

        if len > 1
            && len < 40
            && line.ends_with(':')
            && !rows.is_empty()
            && line.starts_with(|c: char| c.is_ascii_alphabetic())
            && !line.contains('\x1b')
        {
            break;
        }

        if rows.len() >= MAX_LOGO_ROWS {
            break;
        }
        rows.push(line.to_string());
    }

    let rows = trim_logo_rows(rows);
    if rows.is_empty() {
        None
    } else {
        Some(rows)
    }
}

fn load_logo_fastfetch(name: &str) -> Option<Vec<String>> {
    load_logo_ff_colored(name).or_else(|| load_logo_ff_plain(name))
}

// Logo export
//
// logo.txt is the only logo source that costs nothing at render time — no
// fastfetch, no network, just a file read. So both exporters below write that
// format and nothing else: a `# distro:` hint line, then the art.
//
// Neither one writes the file itself. Printing to stdout keeps them composable
// and means a typo in the name can't clobber art you spent an evening on.

const LOGO_BASE_URL: &str = "https://raw.githubusercontent.com/elwrcl/nyi/main/logos";

/// $NYI_LOGO_URL points --fetch-logo somewhere else: your own fork, an
/// internal mirror, or a file:// path when you're testing.
fn logo_base_url() -> String {
    match std::env::var("NYI_LOGO_URL") {
        Ok(u) if is_url_safe(&u) => u.trim_end_matches('/').to_string(),
        _ => LOGO_BASE_URL.to_string(),
    }
}

/// the URL gets single-quoted into a shell command, so the one byte that
/// actually matters is the quote itself.
fn is_url_safe(s: &str) -> bool {
    !s.is_empty() && s.len() < 512 && s.bytes().all(|c| c.is_ascii_graphic() && c != b'\'')
}

fn print_logo_file(name: &str, rows: &[String]) {
    println!("# distro: {name}");
    for r in rows {
        println!("{r}");
    }
}

/// true if `name` is an executable on PATH.
fn have_bin(name: &str) -> bool {
    is_shell_safe(name) && which(name).is_some()
}

/// write `name`'s logo to stdout in logo.txt form, from fastfetch or the
/// built-in table. Run this on a machine that has fastfetch, keep the output.
fn dump_logo(name: &str) -> Result<(), String> {
    let rows = ff_knows_logo(name)
        .then(|| load_logo_fastfetch(name))
        .flatten()
        .or_else(|| builtin_logo(name))
        .ok_or_else(|| format!("nyi: no {name} logo found (install fastfetch for the full set)"))?;
    print_logo_file(name, &rows);
    Ok(())
}

/// download `name`'s logo and write it to stdout.
///
/// For machines that can't run fastfetch at all — iSH, a Termux install you
/// don't want to grow, a container. This is the only code path in nyi that
/// touches the network, it runs only when you name it on the command line, and
/// it shells out rather than linking a TLS stack: one libc dependency is the
/// whole point of the binary being 600K.
fn fetch_logo(name: &str) -> Result<(), String> {
    if !is_shell_safe(name) {
        return Err(format!("nyi: '{name}' is not a valid logo name"));
    }
    // logos/ is lowercase because a URL path is case-sensitive and the name we
    // are handed comes from whatever the machine calls itself.
    let url = format!("{}/{}.txt", logo_base_url(), name.to_ascii_lowercase());
    let cmd = if have_bin("curl") {
        format!("curl -fsSL --max-time 20 '{url}'")
    } else if have_bin("wget") {
        format!("wget -qO- --timeout=20 '{url}'")
    } else {
        return Err("nyi: --fetch-logo needs curl or wget on PATH".to_string());
    };

    let text = run(&cmd)
        .filter(|t| !t.trim().is_empty())
        .ok_or_else(|| format!("nyi: could not fetch {url}"))?;

    // it arrives already in logo.txt form, but it arrived over the wire, so it
    // goes through the same trimming and row cap as anything else.
    let mut rows: Vec<String> = Vec::new();
    let mut distro = name.to_string();
    for line in text.split('\n') {
        let line = strip_eol(line);
        if rows.is_empty() {
            if let Some(v) = line.strip_prefix("# distro:") {
                let v = v.trim_start_matches(' ');
                if !v.is_empty() {
                    distro = v.chars().take(63).collect();
                }
                continue;
            }
            if line.is_empty() {
                continue;
            }
        }
        if rows.len() >= MAX_LOGO_ROWS {
            break;
        }
        rows.push(line.chars().take(MAX_LOGO_COLS * 8).collect());
    }
    let rows = trim_logo_rows(rows);
    if rows.is_empty() {
        return Err(format!("nyi: {url} held no logo"));
    }
    print_logo_file(&distro, &rows);
    Ok(())
}

fn json_str_after(s: &str) -> Option<String> {
    let colon = s.find(':')?;
    let rest = &s[colon..];
    let q1 = rest.find('"')? + 1;
    let q2 = rest[q1..].find('"')? + q1;
    if q2 == q1 || q2 - q1 >= 64 {
        return None;
    }
    Some(rest[q1..q2].to_string())
}
// detect distro using fastfetch, if available. Returns (id, id_like).
//
// Only the OS module: a bare `--json` runs every module fastfetch has, package
// counts and all, which made this one call the slowest thing nyi did at startup.
fn detect_distro_fastfetch() -> (Option<String>, Option<String>) {
    if !have_bin("fastfetch") {
        return (None, None);
    }
    let Some(text) = run_argv("fastfetch", &["-s", "OS", "--format", "json"]) else {
        return (None, None);
    };
    let mut found_os = false;
    let mut id = None;
    let mut id_like = None;
    for line in text.lines() {
        if line.contains("\"OS\"") {
            found_os = true;
        }
        if !found_os {
            continue;
        }
        if let Some(p) = line.find("\"id\"") {
            if let Some(v) = json_str_after(&line[p..]) {
                id = Some(v);
            }
        }
        if let Some(p) = line.find("\"idLike\"") {
            if let Some(v) = json_str_after(&line[p..]) {
                id_like = Some(v);
            }
        }
        // "id" comes before "idLike", so read to the end of the object
        if id.is_some() && line.trim_start().starts_with('}') {
            break;
        }
    }
    (id, id_like)
}

/// Strip quotes and trailing newlines off an os-release value.
#[cfg(not(target_os = "macos"))]
fn os_release_val(raw: &str) -> Option<String> {
    let mut v = strip_eol(raw);
    v = v.strip_prefix('"').unwrap_or(v);
    v = v.strip_suffix('"').unwrap_or(v);
    if v.is_empty() || v.len() >= 64 {
        None
    } else {
        Some(v.to_string())
    }
}

#[cfg(not(target_os = "macos"))]
fn detect_distro_os_release() -> (Option<String>, Option<String>) {
    let Some(text) = read_file("/etc/os-release") else {
        return (None, None);
    };
    let mut id = None;
    let mut id_like = None;
    for line in text.split('\n') {
        if id.is_none() {
            if let Some(v) = line.strip_prefix("ID=") {
                id = os_release_val(v);
                continue;
            }
        }
        if let Some(v) = line.strip_prefix("ID_LIKE=") {
            id_like = os_release_val(v);
        }
    }
    (id, id_like)
}

#[cfg(target_os = "macos")]
fn detect_distro() -> (Option<String>, Option<String>) {
    let (id, like) = detect_distro_fastfetch();
    if id.is_some() {
        return (id, like);
    }
    if let Some(o) = run("sw_vers -productName 2>/dev/null") {
        let name = strip_eol(o.split('\n').next().unwrap_or(""));
        if !name.is_empty() && name.len() < 64 {
            return (Some(name.to_string()), like);
        }
    }
    (Some("macos".to_string()), like)
}

#[cfg(not(target_os = "macos"))]
fn detect_distro() -> (Option<String>, Option<String>) {
    if mobile::is_android() {
        return (Some("android".to_string()), None);
    }
    let (id, like) = detect_distro_fastfetch();
    if id.is_some() {
        return (id, like);
    }
    // iSH runs on Alpine, so fall back to its logo.
    if mobile::is_ish() {
        return (Some("ish".to_string()), Some("alpine".to_string()));
    }
    let (id2, like2) = detect_distro_os_release();
    (id2, like2.or(like))
}

/// Two-tone colors for logos that carry no ANSI of their own.
fn distro_colors(distro: &str) -> (&'static str, &'static str) {
    // (outer, inner)
    if eq_ci(distro, "gentoo") {
        ("\x1b[1;35m", "\x1b[1;37m")
    } else if eq_ci(distro, "android") || eq_ci(distro, "termux") {
        ("\x1b[1;32m", "\x1b[1;37m")
    } else if eq_ci(distro, "ish") || eq_ci(distro, "ios") || eq_ci(distro, "ipados") {
        ("\x1b[1;37m", "\x1b[0;37m")
    } else if eq_ci(distro, "alpine") {
        ("\x1b[1;34m", "\x1b[1;37m")
    } else if eq_ci(distro, "arch") {
        ("\x1b[1;36m", "\x1b[1;36m")
    // bold red / bold white, shared by Ubuntu, Debian and the Asahi remixes
    } else if eq_ci(distro, "ubuntu")
        || eq_ci(distro, "debian")
        || eq_ci(distro, "asahi")
        || eq_ci(distro, "asahi2")
        || eq_ci(distro, "fedora-asahi-remix")
    {
        ("\x1b[1;31m", "\x1b[1;37m")
    } else if eq_ci(distro, "fedora") || distro.to_ascii_lowercase().starts_with("fedora-") {
        ("\x1b[1;34m", "\x1b[1;37m")
    } else if eq_ci(distro, "nixos") {
        ("\x1b[1;34m", "\x1b[1;36m")
    } else if eq_ci(distro, "void") {
        ("\x1b[1;32m", "\x1b[1;32m")
    } else if eq_ci(distro, "opensuse-tumbleweed")
        || eq_ci(distro, "opensuse-leap")
        || eq_ci(distro, "opensuse")
    {
        ("\x1b[1;32m", "\x1b[1;37m")
    } else if eq_ci(distro, "macos") {
        ("\x1b[1;36m", "\x1b[1;37m")
    } else {
        (DEFAULT_COLOR_OUTER, DEFAULT_COLOR_INNER)
    }
}

// Config

#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(usize)]
enum Field {
    Os,
    Host,
    Kernel,
    Uptime,
    Packages,
    Shell,
    Display,
    Wm,
    Theme,
    Icons,
    Font,
    Cursor,
    Terminal,
    Cpu,
    Gpu,
    Memory,
    Swap,
    Disk,
    Ip,
    Battery,
    Locale,
    Audio,
    Colors,
}

const F_COUNT: usize = 23;

const DEFAULT_ORDER: [Field; F_COUNT] = [
    Field::Os,
    Field::Host,
    Field::Kernel,
    Field::Uptime,
    Field::Packages,
    Field::Shell,
    Field::Display,
    Field::Wm,
    Field::Theme,
    Field::Icons,
    Field::Font,
    Field::Cursor,
    Field::Terminal,
    Field::Cpu,
    Field::Gpu,
    Field::Memory,
    Field::Swap,
    Field::Disk,
    Field::Ip,
    Field::Battery,
    Field::Locale,
    Field::Audio,
    Field::Colors,
];

/// the panel style's default: the summary line's fields first (they don't get
/// rows of their own), then the rows top to bottom.
const PANEL_ORDER: [Field; 16] = [
    Field::Os,
    Field::Kernel,
    Field::Packages,
    Field::Wm,
    Field::Host,
    Field::Cpu,
    Field::Gpu,
    Field::Memory,
    Field::Disk,
    Field::Battery,
    Field::Display,
    Field::Ip,
    Field::Audio,
    Field::Shell,
    Field::Terminal,
    Field::Uptime,
];

const FIELD_NAMES: [(&str, Field); F_COUNT] = [
    ("os", Field::Os),
    ("host", Field::Host),
    ("kernel", Field::Kernel),
    ("uptime", Field::Uptime),
    ("packages", Field::Packages),
    ("shell", Field::Shell),
    ("display", Field::Display),
    ("wm", Field::Wm),
    ("theme", Field::Theme),
    ("icons", Field::Icons),
    ("font", Field::Font),
    ("cursor", Field::Cursor),
    ("terminal", Field::Terminal),
    ("cpu", Field::Cpu),
    ("gpu", Field::Gpu),
    ("memory", Field::Memory),
    ("swap", Field::Swap),
    ("disk", Field::Disk),
    ("ip", Field::Ip),
    ("battery", Field::Battery),
    ("locale", Field::Locale),
    ("audio", Field::Audio),
    ("colors", Field::Colors),
];

impl Field {
    fn idx(self) -> usize {
        self as usize
    }
    fn from_name(name: &str) -> Option<Field> {
        FIELD_NAMES
            .iter()
            .find(|(n, _)| eq_ci(name, n))
            .map(|(_, f)| *f)
    }
}

/// C's atoi(): longest leading integer, 0 if there isn't one.
fn atoi(s: &str) -> i32 {
    let s = s.trim_start();
    let b = s.as_bytes();
    let mut i = 0;
    if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
        i += 1;
    }
    let start_digits = i;
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
    }
    if i == start_digits {
        return 0;
    }
    s[..i].parse::<i32>().unwrap_or(0)
}

/// C's atof(): longest leading decimal, 0.0 if there isn't one.
fn atof(s: &str) -> f32 {
    let s = s.trim_start();
    let b = s.as_bytes();
    let mut i = 0;
    if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
        i += 1;
    }
    let mut seen = false;
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
        seen = true;
    }
    if i < b.len() && b[i] == b'.' {
        i += 1;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
            seen = true;
        }
    }
    if !seen {
        return 0.0;
    }
    s[..i].parse::<f32>().unwrap_or(0.0)
}

/// Color names to their ANSI number. Anything else passes through, so
/// `label_color=38;5;213` works.
fn color_code(val: &str) -> String {
    match val {
        "red" => "31",
        "green" => "32",
        "yellow" => "33",
        "blue" => "34",
        "magenta" => "35",
        "cyan" => "36",
        "white" => "37",
        other => other,
    }
    .to_string()
}

fn bold_color_escape(val: &str) -> String {
    format!("\x1b[1;{}m", color_code(val))
}

/// Light direction presets.
fn light_preset(name: &str) -> Option<(f32, f32, f32)> {
    Some(match name {
        "top-left" => (0.41, 0.82, -0.41),
        "top-right" => (-0.41, 0.82, -0.41),
        "top" => (0.0, 0.89, -0.45),
        "left" => (0.82, 0.41, -0.41),
        "right" => (-0.82, 0.41, -0.41),
        "front" => (0.0, 0.0, -1.0),
        "bottom-left" => (0.41, -0.82, -0.41),
        "bottom-right" => (-0.41, -0.82, -0.41),
        _ => return None,
    })
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Style {
    /// `user@host`, an underline and `Label: value` lines, neofetch-style
    Classic,
    /// a header, a `·`-joined summary line, and dim labels over values with
    /// usage bars
    Panel,
}

impl Style {
    fn from_name(name: &str) -> Option<Style> {
        match name {
            "classic" => Some(Style::Classic),
            "panel" => Some(Style::Panel),
            _ => None,
        }
    }
}

struct Config {
    order: Vec<Field>,
    enabled: [bool; F_COUNT],
    label_color: String,
    /// label_color came from the config, not the default
    label_color_set: bool,
    /// 0 = auto (match info lines)
    height: i32,
    size_scale: f32,
    /// 0 = use flag/default
    speed: f32,
    /// none = use flag/default
    spin: Option<(bool, bool)>,
    box_on: bool,
    shading: Option<String>,
    shading_mode: Option<String>,
    separator: String,
    depth: f32,
    depth_user_set: bool,
    logo_outer: Option<String>,
    logo_inner: Option<String>,
    light: (f32, f32, f32),
    extra_disks: Vec<String>,
    style: Style,
    /// none = the style's default (on for panel)
    footer: Option<bool>,
    /// the config file named fields itself, so the style's default order is
    /// not used
    fields_listed: bool,
}

impl Default for Config {
    fn default() -> Self {
        let order = DEFAULT_ORDER.to_vec();
        let mut enabled = [false; F_COUNT];
        for f in &order {
            enabled[f.idx()] = true;
        }
        Config {
            order,
            enabled,
            label_color: "35".to_string(), // default magenta
            label_color_set: false,
            height: 0,
            size_scale: 1.0,
            speed: 0.0,
            spin: None,
            box_on: false,
            shading: None,
            shading_mode: None,
            separator: "-".to_string(),
            depth: 1.0,
            depth_user_set: false,
            logo_outer: None,
            logo_inner: None,
            light: (0.4082, 0.8165, -0.4082),
            extra_disks: Vec::new(),
            style: Style::Panel,
            footer: None,
            fields_listed: false,
        }
    }
}

impl Config {
    fn enable(&mut self, f: Field) {
        if !self.enabled[f.idx()] {
            self.enabled[f.idx()] = true;
            self.order.push(f);
        }
    }

    /// read the config file. A file that exists replaces the default field
    fn load(&mut self, path: &str) -> bool {
        let Some(text) = read_file(path) else {
            return false;
        };

        self.enabled = [false; F_COUNT];
        self.order.clear();

        for raw in text.split('\n') {
            let line = raw
                .trim_end_matches(['\n', '\r', ' '])
                .trim_start_matches([' ', '\t']);
            // Skip comments and empty lines
            if line.is_empty() || line.starts_with('#') {
                continue;
            }

            if let Some(v) = line.strip_prefix("label_color=") {
                self.label_color = color_code(strip_inline_hint(v));
                self.label_color_set = true;
            } else if let Some(v) = line.strip_prefix("height=") {
                self.height = atoi(strip_inline_hint(v)).min(MAX_HEIGHT);
            } else if let Some(v) = line.strip_prefix("size=") {
                self.size_scale = atof(strip_inline_hint(v)).clamp(0.5, 5.0);
            } else if let Some(v) = line.strip_prefix("speed=") {
                self.speed = atof(strip_inline_hint(v));
            } else if let Some(v) = line.strip_prefix("spin=") {
                let v = strip_inline_hint(v);
                self.spin = Some((v.contains(['x', 'X']), v.contains(['y', 'Y'])));
            } else if let Some(v) = line.strip_prefix("box=") {
                self.box_on = is_truthy(strip_inline_hint(v));
            } else if let Some(v) = line.strip_prefix("footer=") {
                self.footer = Some(is_truthy(strip_inline_hint(v)));
            } else if let Some(v) = line.strip_prefix("style=") {
                if let Some(st) = Style::from_name(strip_inline_hint(v)) {
                    self.style = st;
                }
            } else if let Some(v) = line.strip_prefix("shading_mode=") {
                // an empty value reads as "unset", not as a mode named ""
                self.shading_mode =
                    Some(strip_inline_hint(v).to_string()).filter(|m| !m.is_empty());
            } else if let Some(v) = line.strip_prefix("shading=") {
                // no strip_inline_hint() here, shading ramps are freeform
                self.shading = Some(v.to_string()).filter(|r| !r.is_empty());
            } else if let Some(v) = line.strip_prefix("separator=") {
                // no strip_inline_hint() here, separators are freeform
                self.separator = v.to_string();
            } else if let Some(v) = line.strip_prefix("depth=") {
                self.depth = atof(strip_inline_hint(v)).clamp(0.1, 10.0);
                self.depth_user_set = true;
            } else if let Some(v) = line.strip_prefix("logo_outer=") {
                self.logo_outer = Some(bold_color_escape(strip_inline_hint(v)));
            } else if let Some(v) = line.strip_prefix("logo_inner=") {
                self.logo_inner = Some(bold_color_escape(strip_inline_hint(v)));
            } else if let Some(v) = line.strip_prefix("light=") {
                if let Some(l) = light_preset(strip_inline_hint(v)) {
                    self.light = l;
                }
            } else if line.len() >= 5 && line.as_bytes()[..5].eq_ignore_ascii_case(b"disk=") {
                // no strip_inline_hint() here, paths may contain spaces
                let path = &line[5..];
                if !path.is_empty() && self.extra_disks.len() < MAX_EXTRA_DISKS {
                    self.extra_disks.push(path.chars().take(127).collect());
                }
                self.enable(Field::Disk);
            } else if let Some(f) = Field::from_name(line) {
                self.enable(f);
                self.fields_listed = true;
            }
        }
        true
    }

    /// once the file and the flags are both in: a config that only holds
    /// settings keeps the style's default fields, rather than showing none.
    fn finalize(&mut self) {
        if self.fields_listed {
            return;
        }
        let had_disk = self.enabled[Field::Disk.idx()];
        let defaults: &[Field] = match self.style {
            Style::Classic => &DEFAULT_ORDER,
            Style::Panel => &PANEL_ORDER,
        };
        self.order.clear();
        self.enabled = [false; F_COUNT];
        for f in defaults {
            self.enable(*f);
        }
        // disk= lines ask for the field even when the defaults leave it out
        if had_disk {
            self.enable(Field::Disk);
        }
    }

    fn footer_on(&self) -> bool {
        self.footer.unwrap_or(self.style == Style::Panel)
    }
}

fn is_truthy(v: &str) -> bool {
    v == "1" || eq_ci(v, "y") || eq_ci(v, "yes") || eq_ci(v, "true") || eq_ci(v, "on")
}

// Info lines

struct Info {
    lines: Vec<String>,
    field_line: [i32; F_COUNT],
    current: Option<Field>,
    refresh: bool,
    box_width: usize,
    label_color: String,
    style: Style,
    /// lines above the fields (title and underline, or the panel header),
    /// which the box leaves outside
    header_lines: usize,
}

impl Info {
    fn new(cfg: &Config) -> Info {
        Info {
            lines: Vec::new(),
            field_line: [-1; F_COUNT],
            current: None,
            refresh: false,
            box_width: 0,
            label_color: cfg.label_color.clone(),
            style: cfg.style,
            header_lines: 0,
        }
    }

    fn add_line(&mut self, line: &str) {
        if self.lines.len() >= MAX_INFO_LINES {
            return;
        }
        self.lines.push(line.to_string());
    }

    /// Format a labeled info line using the configured label color.
    fn add_info(&mut self, label: &str, value: &str) {
        let line = match self.style {
            Style::Classic => format!("\x1b[1;{}m{}\x1b[0m: {}", self.label_color, label, value),
            Style::Panel => panel::row(label, value),
        };

        if self.refresh {
            if let Some(f) = self.current {
                let idx = self.field_line[f.idx()];
                if idx >= 0 && (idx as usize) < self.lines.len() {
                    let idx = idx as usize;
                    self.lines[idx] = if self.box_width > 0 {
                        let pad = self.box_width.saturating_sub(visible_width(&line));
                        format!("│ {}{} │", line, " ".repeat(pad))
                    } else {
                        line
                    };
                    return;
                }
            }
        }
        if let Some(f) = self.current {
            self.field_line[f.idx()] = self.lines.len() as i32;
        }
        self.add_line(&line);
    }

    /// wrap the info lines
    fn box_wrap_lines(&mut self) {
        let start = self.header_lines;
        if start >= self.lines.len() {
            return;
        }

        let max_w = self.lines[start..]
            .iter()
            .map(|l| visible_width(l))
            .max()
            .unwrap_or(0);
        self.box_width = max_w;

        let content: Vec<String> = self.lines[start..].to_vec();
        let rule = "─".repeat(max_w + 2);

        self.lines.truncate(start);
        self.lines.push(format!("╭{rule}╮"));
        for c in content {
            if self.lines.len() >= MAX_INFO_LINES {
                break;
            }
            let pad = max_w.saturating_sub(visible_width(&c));
            self.lines.push(format!("│ {}{} │", c, " ".repeat(pad)));
        }
        if self.lines.len() < MAX_INFO_LINES {
            self.lines.push(format!("╰{rule}╯"));
        }

        for f in self.field_line.iter_mut() {
            if *f >= start as i32 {
                *f += 1;
            }
        }
    }

    fn widest(&self) -> usize {
        self.lines
            .iter()
            .map(|l| visible_width(l))
            .max()
            .unwrap_or(0)
    }
}

// Platform primitives

#[cfg(target_os = "macos")]
fn sysctl_str(name: &str) -> Option<String> {
    let cname = CString::new(name).ok()?;
    let mut len: libc::size_t = 0;
    unsafe {
        if libc::sysctlbyname(
            cname.as_ptr(),
            std::ptr::null_mut(),
            &mut len,
            std::ptr::null_mut(),
            0,
        ) != 0
            || len == 0
        {
            return None;
        }
        let mut buf = vec![0u8; len];
        if libc::sysctlbyname(
            cname.as_ptr(),
            buf.as_mut_ptr() as *mut libc::c_void,
            &mut len,
            std::ptr::null_mut(),
            0,
        ) != 0
        {
            return None;
        }
        buf.truncate(len);
        while buf.last() == Some(&0) {
            buf.pop();
        }
        let s = String::from_utf8_lossy(&buf).into_owned();
        if s.is_empty() {
            None
        } else {
            Some(s)
        }
    }
}

#[cfg(target_os = "macos")]
fn sysctl_num<T: Copy>(name: &str) -> T {
    unsafe {
        let mut val: T = std::mem::zeroed();
        let Ok(cname) = CString::new(name) else {
            return val;
        };
        let mut len = std::mem::size_of::<T>() as libc::size_t;
        if libc::sysctlbyname(
            cname.as_ptr(),
            &mut val as *mut T as *mut libc::c_void,
            &mut len,
            std::ptr::null_mut(),
            0,
        ) != 0
        {
            return std::mem::zeroed();
        }
        val
    }
}

/// command name of a pid, from /proc on Linux and libproc on macOS.
#[cfg(not(target_os = "macos"))]
fn proc_comm(pid: i32) -> Option<String> {
    read_first_line(&format!("/proc/{pid}/comm"))
}

#[cfg(target_os = "macos")]
fn proc_bsd_info(pid: i32) -> Option<libc::proc_bsdinfo> {
    unsafe {
        let mut info: libc::proc_bsdinfo = std::mem::zeroed();
        let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
        if libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            &mut info as *mut _ as *mut libc::c_void,
            size,
        ) != size
        {
            return None;
        }
        Some(info)
    }
}

#[cfg(target_os = "macos")]
fn proc_comm(pid: i32) -> Option<String> {
    let info = proc_bsd_info(pid)?;
    // pbi_comm is MAXCOMLEN bytes and isn't guaranteed to be terminated.
    let bytes: Vec<u8> = info
        .pbi_comm
        .iter()
        .take_while(|&&c| c != 0)
        .map(|&c| c as u8)
        .collect();
    let s = String::from_utf8_lossy(&bytes).into_owned();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

/// parent pid of a pid.
#[cfg(not(target_os = "macos"))]
fn proc_ppid(pid: i32) -> Option<i32> {
    let text = read_file(&format!("/proc/{pid}/status"))?;
    for line in text.split('\n') {
        if let Some(v) = line.strip_prefix("PPid:") {
            return v.trim().parse::<i32>().ok().filter(|p| *p > 0);
        }
    }
    None
}

#[cfg(target_os = "macos")]
fn proc_ppid(pid: i32) -> Option<i32> {
    let ppid = proc_bsd_info(pid)?.pbi_ppid as i32;
    if ppid > 0 {
        Some(ppid)
    } else {
        None
    }
}

/// entries in a directory that don't start with a dot.
fn dir_entries(path: &str) -> Vec<String> {
    let Ok(rd) = fs::read_dir(path) else {
        return Vec::new();
    };
    rd.filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| !n.starts_with('.'))
        .collect()
}

fn count_entries(path: &str) -> usize {
    dir_entries(path).len()
}

fn statvfs_of(path: &str) -> Option<libc::statvfs> {
    let c = CString::new(path).ok()?;
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c.as_ptr(), &mut st) } != 0 {
        return None;
    }
    Some(st)
}

// Info gathering

fn gather_title(info: &mut Info, cfg: &Config) {
    let user = login_name();
    let host = hostname();
    let lc = info.label_color.clone();
    info.add_line(&format!(
        "\x1b[1;{lc}m{user}\x1b[0m@\x1b[1;{lc}m{host}\x1b[0m"
    ));

    // separator
    let title_len = user.len() + 1 + host.len();
    let sep = if cfg.separator.is_empty() {
        String::new()
    } else {
        cfg.separator.repeat(title_len)
    };
    info.add_line(&sep);
}

fn gather_os(info: &mut Info) {
    let u = uname();
    #[cfg(target_os = "macos")]
    {
        if let Some(version) = sysctl_str("kern.osproductversion") {
            info.add_info("OS", &format!("macOS {} {}", version, u.machine));
        } else {
            info.add_info("OS", &format!("{} {}", u.sysname, u.machine));
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        if mobile::is_android() {
            info.add_info("OS", &format!("{} {}", mobile::os_name(), u.machine));
            return;
        }
        let mut pretty = String::new();
        if let Some(text) = read_file("/etc/os-release") {
            for line in text.split('\n') {
                let Some(v) = line.strip_prefix("PRETTY_NAME=") else {
                    continue;
                };
                let mut v = strip_eol(v);
                if v.starts_with('\'') || v.starts_with('"') {
                    v = &v[1..];
                }
                if v.ends_with('\'') || v.ends_with('"') {
                    v = &v[..v.len() - 1];
                }
                pretty = v.to_string();
                break;
            }
        }
        if pretty.is_empty() {
            pretty = "Linux".to_string();
        }
        // iSH emulates on top of iOS; keep it apart from the distro it runs.
        if mobile::is_ish() {
            pretty = format!("{pretty} on iSH");
        }
        info.add_info("OS", &format!("{} {}", pretty, u.machine));
    }
}

fn gather_host(info: &mut Info) {
    #[cfg(target_os = "macos")]
    {
        if let Some(model) = sysctl_str("hw.model") {
            info.add_info("Host", &model);
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        if mobile::is_android() {
            if let Some(host) = mobile::host() {
                info.add_info("Host", &host);
            }
            return;
        }
        // Try device-tree first (ARM/Apple Silicon), then DMI (x86)
        let model = read_first_line("/proc/device-tree/model")
            .or_else(|| read_first_line("/sys/class/dmi/id/product_name"));
        if let Some(model) = model {
            info.add_info("Host", &model);
        }
    }
}

fn gather_kernel(info: &mut Info) {
    let u = uname();
    info.add_info("Kernel", &format!("{} {}", u.sysname, u.release));
}

/// seconds since boot.
fn uptime_secs() -> Option<i64> {
    #[cfg(target_os = "macos")]
    {
        let bt: libc::timeval = sysctl_num("kern.boottime");
        if bt.tv_sec == 0 {
            return None;
        }
        let now = unsafe { libc::time(std::ptr::null_mut()) };
        Some(now as i64 - bt.tv_sec)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let text = read_file("/proc/uptime")?;
        let secs = text
            .split_ascii_whitespace()
            .next()
            .and_then(|v| v.parse::<f64>().ok())
            .unwrap_or(0.0);
        Some(secs as i64)
    }
}

fn gather_uptime(info: &mut Info) {
    let Some(total) = uptime_secs() else {
        return;
    };
    let days = total / 86400;
    let hours = (total % 86400) / 3600;
    let mins = (total % 3600) / 60;
    let s = |n: i64| if n == 1 { "" } else { "s" };

    let val = if days > 0 {
        format!(
            "{} day{}, {} hour{}, {} min{}",
            days,
            s(days),
            hours,
            s(hours),
            mins,
            s(mins)
        )
    } else if hours > 0 {
        format!("{} hour{}, {} min{}", hours, s(hours), mins, s(mins))
    } else {
        format!("{} min{}", mins, s(mins))
    };
    info.add_info("Uptime", &val);
}

/// count packages in an emerge-style two-level dir (category/package).
#[cfg(not(target_os = "macos"))]
fn count_emerge_pkgs() -> usize {
    dir_entries("/var/db/pkg")
        .iter()
        .map(|cat| count_entries(&format!("/var/db/pkg/{cat}")))
        .sum()
}

/// count lines in a file, optionally only those with a given prefix.
#[cfg(not(target_os = "macos"))]
fn count_file_lines(path: &str, prefix: Option<&str>) -> usize {
    let Some(text) = read_file(path) else {
        return 0;
    };
    text.lines()
        .filter(|l| match prefix {
            None => true,
            Some(p) => l.starts_with(p),
        })
        .count()
}

/// nix package counts, on NixOS and on any distro with Nix installed, as
/// (system, user).
///
/// Asking nix-store costs a few hundred milliseconds, so the answer is cached
/// under the store paths the profiles resolve to: it only changes when one of
/// them does, which is a rebuild or a profile switch.
fn count_nix_pkgs() -> (i64, i64) {
    let sys_path = fs::canonicalize("/run/current-system/sw").ok();
    let base = home_dir()
        .map(|h| h.rsplit('/').next().unwrap_or("").to_string())
        .filter(|b| {
            !b.is_empty()
                && b.bytes()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-'))
        });
    let user_path = base
        .as_ref()
        .and_then(|u| fs::canonicalize(format!("/etc/profiles/per-user/{u}")).ok());
    if sys_path.is_none() && user_path.is_none() {
        return (0, 0);
    }
    if !have_bin("nix-store") {
        return (0, 0);
    }
    let show = |p: &Option<std::path::PathBuf>| {
        p.as_ref()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default()
    };
    let refs = |path: &str| -> Vec<String> {
        run_argv("nix-store", &["-q", "--references", path])
            .map(|o| o.lines().map(str::to_string).collect())
            .unwrap_or_default()
    };

    let key = format!("nix2:{}:{}", show(&sys_path), show(&user_path));
    let val = cache::get_or(&key, || {
        let sys = sys_path
            .as_ref()
            .map_or(0, |p| refs(&p.to_string_lossy()).len());
        let usr = match &user_path {
            Some(p) => {
                // The per-user profile wraps the buildEnv that actually holds
                // the packages (home-manager-path under home-manager), next
                // to a few loose files. Without one, its own references are
                // the packages.
                let hop1 = refs(&p.to_string_lossy());
                let env = hop1
                    .iter()
                    .find(|r| r.ends_with("-home-manager-path"))
                    .or_else(|| (hop1.len() == 1).then(|| &hop1[0]));
                match env {
                    Some(h) => refs(h).len(),
                    None => hop1.len(),
                }
            }
            None => 0,
        };
        format!("{sys} {usr}")
    });
    let mut it = val.split_ascii_whitespace();
    let sys = it.next().and_then(|n| n.parse().ok()).unwrap_or(0);
    let usr = it.next().and_then(|n| n.parse().ok()).unwrap_or(0);
    (sys, usr)
}

/// installed flatpak refs, counted off the disk the way `flatpak list` shows
/// them (no .Locale/.Debug extensions), instead of starting flatpak itself.
fn count_flatpak_refs() -> i64 {
    let mut roots = vec!["/var/lib/flatpak".to_string()];
    if let Some(h) = home_dir() {
        roots.push(format!("{h}/.local/share/flatpak"));
    }
    let mut n = 0i64;
    for root in &roots {
        for kind in ["app", "runtime"] {
            let kdir = format!("{root}/{kind}");
            for name in dir_entries(&kdir) {
                if name.ends_with(".Locale") || name.ends_with(".Debug") {
                    continue;
                }
                for arch in dir_entries(&format!("{kdir}/{name}")) {
                    if arch == "current" {
                        continue;
                    }
                    n += count_entries(&format!("{kdir}/{name}/{arch}")) as i64;
                }
            }
        }
    }
    n
}

/// every package manager that has something installed, as (count, manager).
fn package_entries() -> Vec<(i64, String)> {
    let mut v: Vec<(i64, String)> = Vec::new();

    #[cfg(target_os = "macos")]
    {
        // Homebrew — check both ARM and Intel Cellar paths
        let total: usize = ["/opt/homebrew/Cellar", "/usr/local/Cellar"]
            .iter()
            .map(|p| count_entries(p))
            .sum();
        if total > 0 {
            v.push((total as i64, "brew".to_string()));
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        // "N (dpkg)" from termux's own database
        if let Some(m) = mobile::package_count() {
            let n = atoi(&m) as i64;
            if n > 0 {
                v.push((n, "dpkg".to_string()));
            }
        }
        // the distro's native manager; the first one that has anything wins
        let native = || -> Option<(i64, &str)> {
            let n = count_emerge_pkgs();
            if n > 0 {
                return Some((n as i64, "emerge"));
            }
            // -1 for ALPM_DB_VERSION
            let n = count_entries("/var/lib/pacman/local");
            if n > 1 {
                return Some((n as i64 - 1, "pacman"));
            }
            let n = count_file_lines("/var/lib/dpkg/status", Some("Package:"));
            if n > 0 {
                return Some((n as i64, "dpkg"));
            }
            if have_bin("rpm") {
                let n = run_argv("rpm", &["-qa"]).map_or(0, |o| o.lines().count());
                if n > 0 {
                    return Some((n as i64, "rpm"));
                }
            }
            if have_bin("xbps-query") {
                let n = run_argv("xbps-query", &["-l"]).map_or(0, |o| o.lines().count());
                if n > 0 {
                    return Some((n as i64, "xbps"));
                }
            }
            let n = count_file_lines("/lib/apk/db/installed", Some("P:"));
            if n > 0 {
                return Some((n as i64, "apk"));
            }
            None
        };
        if v.is_empty() {
            if let Some((n, m)) = native() {
                v.push((n, m.to_string()));
            }
        }
    }

    // Nix (NixOS, or the Nix package manager on other distros).
    let (sys, usr) = count_nix_pkgs();
    if sys > 0 {
        v.push((sys, "system".to_string()));
    }
    if usr > 0 {
        v.push((usr, "user".to_string()));
    }

    let n = count_flatpak_refs();
    if n > 0 {
        v.push((n, "flatpak".to_string()));
    }
    v
}

fn gather_packages(info: &mut Info) {
    let val = package_entries()
        .iter()
        .map(|(n, m)| format!("{n} ({m})"))
        .collect::<Vec<_>>()
        .join(", ");
    if !val.is_empty() {
        info.add_info("Packages", &val);
    }
}

const KNOWN_SHELLS: [&str; 12] = [
    "bash", "zsh", "fish", "dash", "sh", "nu", "elvish", "tcsh", "csh", "xonsh", "ksh", "ion",
];

/// (name, version) of the shell nyi was started from.
fn shell_info() -> Option<(String, String)> {
    // Try the actual running shell from the parent process, since $SHELL is
    // the login shell which may differ (e.g. fish launched from bash).
    let ppid = unsafe { libc::getppid() };
    let from_parent = proc_comm(ppid).filter(|c| KNOWN_SHELLS.contains(&c.as_str()));

    let shell = from_parent
        .or_else(|| std::env::var("SHELL").ok())
        .filter(|s| !s.is_empty())?;
    let name = shell.rsplit('/').next().unwrap_or(&shell).to_string();
    Some((name, cmd_version(&shell)))
}

fn gather_shell(info: &mut Info) {
    let Some((name, version)) = shell_info() else {
        return;
    };
    if version.is_empty() {
        info.add_info("Shell", &name);
    } else {
        info.add_info("Shell", &format!("{name} {version}"));
    }
}

// --- Display (Linux: DRM + EDID) ---

#[cfg(not(target_os = "macos"))]
#[repr(C)]
#[derive(Clone, Copy)]
struct DrmModeInfoMin {
    clock: u32,
    hdisplay: u16,
    hsync_start: u16,
    hsync_end: u16,
    htotal: u16,
    hskew: u16,
    vdisplay: u16,
    vsync_start: u16,
    vsync_end: u16,
    vtotal: u16,
    vscan: u16,
    vrefresh: u32,
    flags: u32,
    type_: u32,
    name: [u8; 32],
}

#[cfg(not(target_os = "macos"))]
#[repr(C)]
#[derive(Clone, Copy)]
struct DrmCrtcMin {
    set_connectors_ptr: u64,
    count_connectors: u32,
    crtc_id: u32,
    fb_id: u32,
    x: u32,
    y: u32,
    gamma_size: u32,
    mode_valid: u32,
    mode: DrmModeInfoMin,
}

#[cfg(not(target_os = "macos"))]
#[repr(C)]
#[derive(Clone, Copy)]
struct DrmEncoderMin {
    encoder_id: u32,
    encoder_type: u32,
    crtc_id: u32,
    possible_crtcs: u32,
    possible_clones: u32,
}

#[cfg(not(target_os = "macos"))]
#[repr(C)]
#[derive(Clone, Copy)]
struct DrmConnectorMin {
    encoders_ptr: u64,
    modes_ptr: u64,
    props_ptr: u64,
    prop_values_ptr: u64,
    count_modes: u32,
    count_props: u32,
    count_encoders: u32,
    encoder_id: u32,
    connector_id: u32,
    connector_type: u32,
    connector_type_id: u32,
    connection: u32,
    mm_width: u32,
    mm_height: u32,
    subpixel: u32,
    pad: u32,
}

/// _IOWR('d', nr, T) — the DRM ioctl encoding, spelled out so we don't need
/// the kernel headers.
#[cfg(not(target_os = "macos"))]
const fn drm_iowr(nr: u32, size: usize) -> libc::c_ulong {
    ((3u32 << 30) | ((size as u32) << 16) | ((b'd' as u32) << 8) | nr) as libc::c_ulong
}

#[cfg(not(target_os = "macos"))]
struct DrmMode {
    w: i32,
    h: i32,
    hz: f32,
    mm_w: i32,
    mm_h: i32,
}

#[cfg(not(target_os = "macos"))]
fn drm_active_mode(card: &str, conn_id: u32) -> DrmMode {
    let mut result = DrmMode {
        w: 0,
        h: 0,
        hz: 0.0,
        mm_w: 0,
        mm_h: 0,
    };
    let Ok(dev) = CString::new(format!("/dev/dri/{card}")) else {
        return result;
    };
    let fd = unsafe { libc::open(dev.as_ptr(), libc::O_RDONLY | libc::O_CLOEXEC) };
    if fd < 0 {
        return result;
    }

    unsafe {
        let mut conn: DrmConnectorMin = std::mem::zeroed();
        let mut scratch: DrmModeInfoMin = std::mem::zeroed();
        conn.connector_id = conn_id;
        conn.modes_ptr = &mut scratch as *mut _ as u64;
        conn.count_modes = 1;

        let get_connector = drm_iowr(0xA7, std::mem::size_of::<DrmConnectorMin>());
        if libc::ioctl(fd, get_connector as _, &mut conn) == 0 {
            if conn.mm_width != 0 && conn.mm_height != 0 {
                result.mm_w = conn.mm_width as i32;
                result.mm_h = conn.mm_height as i32;
            }
            let mut enc: DrmEncoderMin = std::mem::zeroed();
            enc.encoder_id = conn.encoder_id;
            let get_encoder = drm_iowr(0xA6, std::mem::size_of::<DrmEncoderMin>());
            if conn.encoder_id != 0
                && libc::ioctl(fd, get_encoder as _, &mut enc) == 0
                && enc.crtc_id != 0
            {
                let mut crtc: DrmCrtcMin = std::mem::zeroed();
                crtc.crtc_id = enc.crtc_id;
                let get_crtc = drm_iowr(0xA1, std::mem::size_of::<DrmCrtcMin>());
                if libc::ioctl(fd, get_crtc as _, &mut crtc) == 0
                    && crtc.mode_valid != 0
                    && crtc.mode.htotal != 0
                    && crtc.mode.vtotal != 0
                {
                    result.w = crtc.mode.hdisplay as i32;
                    result.h = crtc.mode.vdisplay as i32;
                    let mut r = crtc.mode.clock as f32 * 1000.0
                        / (crtc.mode.htotal as f32 * crtc.mode.vtotal as f32);
                    if crtc.mode.flags & 0x10 != 0 {
                        r *= 2.0; // double scan
                    }
                    if crtc.mode.flags & 0x20 != 0 {
                        r /= 2.0; // interlace
                    }
                    if crtc.mode.vscan > 1 {
                        r /= crtc.mode.vscan as f32;
                    }
                    result.hz = r;
                }
            }
        }
        libc::close(fd);
    }
    result
}

#[cfg(not(target_os = "macos"))]
fn read_edid(conn_dir: &str) -> Option<Vec<u8>> {
    let edid = fs::read(format!("/sys/class/drm/{conn_dir}/edid")).ok()?;
    if edid.len() < 128 {
        return None;
    }
    const HDR: [u8; 8] = [0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00];
    if edid[..8] != HDR {
        return None;
    }
    Some(edid)
}

#[cfg(not(target_os = "macos"))]
fn edid_name(e: &[u8]) -> String {
    // Descriptor blocks at 54, 72, 90, 108; tag 0xFC is the monitor name.
    for i in (54..=108).step_by(18) {
        let d = &e[i..];
        if d.len() < 18 || d[0] != 0 || d[1] != 0 || d[2] != 0 || d[3] != 0xFC {
            continue;
        }
        let mut name = String::new();
        for &c in &d[5..18] {
            if c == 0x0A {
                break;
            }
            name.push(c as char);
        }
        let name = name.trim_end_matches(' ').to_string();
        if !name.is_empty() {
            return name;
        }
    }
    // Fall back to the PnP vendor id plus the product code.
    let v = ((e[8] as u32) << 8) | e[9] as u32;
    let m = [
        (b'A' as u32 + ((v >> 10) & 0x1F) - 1) as u8,
        (b'A' as u32 + ((v >> 5) & 0x1F) - 1) as u8,
        (b'A' as u32 + (v & 0x1F) - 1) as u8,
    ];
    if m.iter().any(|c| !c.is_ascii_uppercase()) {
        return String::new();
    }
    format!("{}{:02X}{:02X}", String::from_utf8_lossy(&m), e[11], e[10])
}

#[cfg(not(target_os = "macos"))]
fn edid_size_mm(e: &[u8]) -> (i32, i32) {
    let d = &e[54..];
    if d[0] != 0 || d[1] != 0 {
        let mw = d[12] as i32 | (((d[14] & 0xF0) as i32) << 4);
        let mh = d[13] as i32 | (((d[14] & 0x0F) as i32) << 8);
        if mw > 0 && mh > 0 {
            return (mw, mh);
        }
    }
    if e[21] != 0 && e[22] != 0 {
        return (e[21] as i32 * 10, e[22] as i32 * 10);
    }
    (0, 0)
}

#[cfg(not(target_os = "macos"))]
fn edid_refresh(e: &[u8]) -> f32 {
    let d = &e[54..];
    let clock = d[0] as u32 | ((d[1] as u32) << 8);
    if clock == 0 {
        return 0.0;
    }
    let htotal = (d[2] as u32 | (((d[4] & 0xF0) as u32) << 4))
        + (d[3] as u32 | (((d[4] & 0x0F) as u32) << 8));
    let vtotal = (d[5] as u32 | (((d[7] & 0xF0) as u32) << 4))
        + (d[6] as u32 | (((d[7] & 0x0F) as u32) << 8));
    if htotal == 0 || vtotal == 0 {
        return 0.0;
    }
    clock as f32 * 10000.0 / (htotal * vtotal) as f32
}

/// Used on both platforms: Linux EDID/DRM, macOS "UI Looks like".
fn format_hz(hz: f32) -> String {
    if hz <= 0.0 {
        return String::new();
    }
    if (hz - hz.round()).abs() < 0.05 {
        format!("{:.0} Hz", hz)
    } else {
        format!("{:.2} Hz", hz)
    }
}

#[cfg(not(target_os = "macos"))]
fn connector_is_internal(conn: &str) -> bool {
    conn.starts_with("eDP") || conn.starts_with("LVDS") || conn.starts_with("DSI")
}

/// `system_profiler SPDisplaysDataType` is expensive (it can take seconds) and
/// carries both display and GPU info in one output. Run it once.
#[cfg(target_os = "macos")]
fn sp_displays_text() -> Option<&'static String> {
    static CACHE: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    CACHE
        .get_or_init(|| run("system_profiler SPDisplaysDataType 2>/dev/null"))
        .as_ref()
}

#[cfg(target_os = "macos")]
const MAX_DISPLAYS: usize = 8;

#[cfg(target_os = "macos")]
#[derive(Default)]
struct MacDisplay {
    name: String,
    /// The panel's real mode. On Retina it differs from the UI size.
    res: String,
    /// "UI Looks like" — the scaled desktop size.
    ui: String,
    hz: f32,
    internal: bool,
}

/// Parse `system_profiler SPDisplaysDataType`. The format is structured by
/// indentation: each GPU is a heading, `Displays:` opens a section, and every
/// heading under it is a display. Display names are bare headings rather than
/// key/value lines, so tracking indentation is the only way to tell them apart.
#[cfg(target_os = "macos")]
fn parse_sp_displays(text: &str) -> (Vec<String>, Vec<MacDisplay>) {
    let mut gpus: Vec<String> = Vec::new();
    let mut displays: Vec<MacDisplay> = Vec::new();
    let mut displays_indent: Option<usize> = None;
    let mut cur: Option<usize> = None;

    for raw in text.lines() {
        let line = raw.trim_end();
        let body = line.trim_start();
        if body.is_empty() {
            continue;
        }
        let indent = line.len() - body.len();

        // A heading: a `Something:` line with no value after it.
        if let Some(head) = body.strip_suffix(':') {
            if head.eq_ignore_ascii_case("Displays") {
                displays_indent = Some(indent);
                cur = None;
            } else if displays_indent.is_some_and(|d| indent > d) {
                if displays.len() < MAX_DISPLAYS {
                    displays.push(MacDisplay {
                        name: head.to_string(),
                        ..Default::default()
                    });
                    cur = Some(displays.len() - 1);
                } else {
                    cur = None;
                }
            } else {
                // Same or shallower level: we've left the Displays section.
                displays_indent = None;
                cur = None;
            }
            continue;
        }

        let Some((key, val)) = body.split_once(':') else {
            continue;
        };
        let (key, val) = (key.trim(), val.trim());
        if val.is_empty() {
            continue;
        }

        if key.eq_ignore_ascii_case("Chipset Model") || key.eq_ignore_ascii_case("Chip") {
            if !gpus.iter().any(|g| g == val) {
                gpus.push(val.to_string());
            }
            continue;
        }

        let Some(i) = cur else {
            continue;
        };
        let d = &mut displays[i];
        if key.eq_ignore_ascii_case("Resolution") {
            // Apple appends a marketing name: "1366 x 768 (WSGA - Wide ...)"
            d.res = normalize_res(val.split(" (").next().unwrap_or(val));
        } else if key.eq_ignore_ascii_case("UI Looks like") {
            // The refresh rate only appears here: "1920 x 1080 @ 60.00Hz"
            match val.split_once('@') {
                Some((r, hz)) => {
                    d.ui = normalize_res(r);
                    d.hz = atof(hz.trim().trim_end_matches("Hz").trim());
                }
                None => d.ui = normalize_res(val),
            }
        } else if key.eq_ignore_ascii_case("Connection Type") {
            d.internal = val.eq_ignore_ascii_case("Internal");
        }
    }
    (gpus, displays)
}

struct DisplayInfo {
    /// EDID or system_profiler name; empty when unknown
    name: String,
    /// connector name (eDP-1, HDMI-A-1) on Linux
    #[cfg_attr(target_os = "macos", allow(dead_code))]
    conn: String,
    res: String,
    /// macOS Retina: the scaled desktop size, when it differs from `res`
    scaled: String,
    hz: f32,
    /// diagonal in inches, 0 when unknown
    inches: i32,
    internal: bool,
}

/// "1366 x 768" -> "1366x768"
#[cfg(target_os = "macos")]
fn normalize_res(s: &str) -> String {
    s.split_whitespace().collect()
}

#[cfg(target_os = "macos")]
fn display_list() -> Vec<DisplayInfo> {
    let Some(text) = sp_displays_text() else {
        return Vec::new();
    };
    let (_, displays) = parse_sp_displays(text);
    displays
        .into_iter()
        .filter_map(|d| {
            let res = if d.res.is_empty() { &d.ui } else { &d.res };
            if res.is_empty() {
                return None;
            }
            // Retina panels report the real mode in Resolution and the scaled
            // desktop in "UI Looks like". Keep both when they differ.
            let scaled = if !d.ui.is_empty() && &d.ui != res {
                d.ui.clone()
            } else {
                String::new()
            };
            // system_profiler gives the built-in panel the generic heading
            // "Display"; that is no name at all.
            let name = if d.name.eq_ignore_ascii_case("Display") {
                String::new()
            } else {
                d.name.clone()
            };
            Some(DisplayInfo {
                name,
                conn: String::new(),
                res: res.clone(),
                scaled,
                hz: d.hz,
                inches: 0,
                internal: d.internal,
            })
        })
        .collect()
}

#[cfg(target_os = "macos")]
fn gather_display(info: &mut Info) {
    let mut generic = 0usize;
    for d in display_list() {
        let scaled = if d.scaled.is_empty() {
            String::new()
        } else {
            format!(" (as {})", d.scaled)
        };
        let rate = format_hz(d.hz);
        let sep = if rate.is_empty() { "" } else { " @ " };
        let kind = if d.internal { "Built-in" } else { "External" };
        let label = if d.name.is_empty() {
            generic += 1;
            if generic == 1 {
                "Display".to_string()
            } else {
                format!("Display {generic}")
            }
        } else {
            format!("Display ({})", d.name)
        };
        info.add_info(&label, &format!("{}{scaled}{sep}{rate} [{kind}]", d.res));
    }
}

#[cfg(not(target_os = "macos"))]
fn display_list() -> Vec<DisplayInfo> {
    let mut out = Vec::new();

    for entry in dir_entries("/sys/class/drm") {
        // Pattern: cardN-CONNECTOR (skip bare cardN and renderD*)
        if !entry.starts_with("card") {
            continue;
        }
        let Some(dash) = entry[4..].find('-').map(|i| i + 4) else {
            continue;
        };

        if read_first_line(&format!("/sys/class/drm/{entry}/status")).as_deref()
            != Some("connected")
        {
            continue;
        }
        let Some(mode) = read_first_line(&format!("/sys/class/drm/{entry}/modes")) else {
            continue;
        };

        let conn = &entry[dash + 1..];
        let card = &entry[..dash];
        let edid = read_edid(&entry);

        let conn_id = read_i64(&format!("/sys/class/drm/{entry}/connector_id")).unwrap_or(0);
        let mut m = if conn_id > 0 {
            drm_active_mode(card, conn_id as u32)
        } else {
            DrmMode {
                w: 0,
                h: 0,
                hz: 0.0,
                mm_w: 0,
                mm_h: 0,
            }
        };

        if let Some(e) = &edid {
            if m.hz <= 0.0 {
                m.hz = edid_refresh(e);
            }
            if m.mm_w == 0 || m.mm_h == 0 {
                let (w, h) = edid_size_mm(e);
                if w != 0 || h != 0 {
                    m.mm_w = w;
                    m.mm_h = h;
                }
            }
        }

        let res = if m.w > 0 && m.h > 0 {
            format!("{}x{}", m.w, m.h)
        } else {
            mode
        };
        let inches = if m.mm_w > 0 && m.mm_h > 0 {
            (((m.mm_w * m.mm_w + m.mm_h * m.mm_h) as f32).sqrt() / 25.4).round() as i32
        } else {
            0
        };

        out.push(DisplayInfo {
            name: edid.as_deref().map(edid_name).unwrap_or_default(),
            conn: conn.to_string(),
            res,
            scaled: String::new(),
            hz: m.hz,
            inches,
            internal: connector_is_internal(conn),
        });
    }

    // Fallback: the first mode from any card
    if out.is_empty() {
        for entry in dir_entries("/sys/class/drm") {
            if !entry.starts_with("card") {
                continue;
            }
            if let Some(mode) = read_first_line(&format!("/sys/class/drm/{entry}/modes")) {
                out.push(DisplayInfo {
                    name: String::new(),
                    conn: String::new(),
                    res: mode,
                    scaled: String::new(),
                    hz: 0.0,
                    inches: 0,
                    internal: false,
                });
                break;
            }
        }
    }
    out
}

#[cfg(not(target_os = "macos"))]
fn gather_display(info: &mut Info) {
    for d in display_list() {
        // the fallback entry knows nothing but a mode
        if d.conn.is_empty() {
            info.add_info("Display", &d.res);
            continue;
        }
        let label = format!(
            "Display ({})",
            if d.name.is_empty() { &d.conn } else { &d.name }
        );
        let inches = if d.inches > 0 {
            format!(" in {}\"", d.inches)
        } else {
            String::new()
        };
        let rate = format_hz(d.hz);
        let sep = if rate.is_empty() {
            ""
        } else if inches.is_empty() {
            " @ "
        } else {
            ", "
        };
        let kind = if d.internal { "Built-in" } else { "External" };
        info.add_info(&label, &format!("{}{inches}{sep}{rate} [{kind}]", d.res));
    }
}

#[cfg(not(target_os = "macos"))]
const KNOWN_WMS: [&str; 10] = [
    "dwl", "sway", "river", "labwc", "weston", "i3", "bspwm", "openbox", "awesome", "dwm",
];

/// (name, version, wayland) of the running window manager.
fn wm_info() -> Option<(String, String, bool)> {
    #[cfg(target_os = "macos")]
    {
        Some(("Aqua".to_string(), String::new(), false))
    }
    #[cfg(not(target_os = "macos"))]
    {
        // WAYLAND_DISPLAY or XDG_SESSION_TYPE tells us the session type
        let wayland = std::env::var("WAYLAND_DISPLAY").unwrap_or_default();
        let session = std::env::var("XDG_SESSION_TYPE").unwrap_or_default();
        let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
        let is_wayland = !wayland.is_empty() || session == "wayland";

        // Process detection is most accurate (e.g. dwl sets
        // XDG_CURRENT_DESKTOP=sway for compat), so try that before the env.
        let mut wm = String::new();
        // wm_is_binary is true only if wm holds an actual invocable binary name
        let mut wm_is_binary = false;

        // 1. Check env vars for specific WMs
        if std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_some() {
            wm = "Hyprland".to_string();
            wm_is_binary = true;
        }

        // 2. Try the process list by scanning /proc/*/comm
        if wm.is_empty() {
            for entry in dir_entries("/proc") {
                if !entry.starts_with(|c: char| ('1'..='9').contains(&c)) {
                    continue;
                }
                let Some(comm) = read_first_line(&format!("/proc/{entry}/comm")) else {
                    continue;
                };
                if KNOWN_WMS.contains(&comm.as_str()) {
                    wm = comm;
                    wm_is_binary = true;
                    break;
                }
            }
        }

        // 3. Fall back to a DE-to-WM mapping from XDG_CURRENT_DESKTOP
        if wm.is_empty() && !desktop.is_empty() {
            let first = desktop.split(':').next().unwrap_or("");
            let mapped = if eq_ci(first, "KDE") {
                Some("KWin")
            } else if eq_ci(first, "GNOME") {
                Some("Mutter")
            } else if eq_ci(first, "XFCE") {
                Some("xfwm4")
            } else if eq_ci(first, "Cinnamon") {
                Some("Muffin")
            } else if eq_ci(first, "MATE") {
                Some("Marco")
            } else if eq_ci(first, "LXQt") {
                Some("Openbox")
            } else if eq_ci(first, "Budgie") {
                Some("Mutter")
            } else if eq_ci(first, "Deepin") {
                Some("KWin")
            } else {
                None
            };
            match mapped {
                Some(m) => wm = m.to_string(),
                None => {
                    // Not a known DE name — likely the WM's own name (e.g.
                    // niri), which is also a real invocable binary.
                    wm = desktop.clone();
                    wm_is_binary = true;
                }
            }
        }

        if wm.is_empty() {
            return None;
        }
        let version = if wm_is_binary {
            cmd_version(&wm)
        } else {
            String::new()
        };
        Some((wm, version, is_wayland))
    }
}

fn gather_wm(info: &mut Info) {
    let Some((wm, version, is_wayland)) = wm_info() else {
        return;
    };
    let suffix = if is_wayland { " (Wayland)" } else { "" };
    if version.is_empty() {
        info.add_info("WM", &format!("{wm}{suffix}"));
    } else {
        info.add_info("WM", &format!("{wm} {version}{suffix}"));
    }
}

/// chip name out of a device-tree model string, e.g. "M1" from
/// "Apple MacBook Air (M1, 2020)".
#[cfg(not(target_os = "macos"))]
fn devicetree_chip() -> Option<String> {
    let model = read_first_line("/proc/device-tree/model")?;
    let paren = model.find('(')? + 1;
    let rest = &model[paren..];
    let end = rest.find(',').or_else(|| rest.find(')'))?;
    let chip = rest[..end].trim();
    if chip.is_empty() {
        None
    } else {
        Some(chip.to_string())
    }
}

struct CpuInfo {
    name: String,
    /// logical CPUs
    threads: i32,
    /// physical cores, 0 when unknown
    cores: i32,
    max_ghz: f32,
    temp_c: Option<f32>,
    /// cpufreq energy_performance_preference, or the governor without one
    profile: Option<String>,
}

/// Package temperature from the first hwmon driver that measures the CPU.
#[cfg(not(target_os = "macos"))]
fn cpu_temp() -> Option<f32> {
    const DRIVERS: [&str; 6] = [
        "coretemp",
        "k10temp",
        "zenpower",
        "cpu_thermal",
        "soc_thermal",
        "cpu0_thermal",
    ];
    let mons: Vec<(String, String)> = dir_entries("/sys/class/hwmon")
        .into_iter()
        .filter_map(|h| {
            let name = read_first_line(&format!("/sys/class/hwmon/{h}/name"))?;
            Some((h, name))
        })
        .collect();
    for drv in DRIVERS {
        let Some((h, _)) = mons.iter().find(|(_, n)| n == drv) else {
            continue;
        };
        let milli = read_i64(&format!("/sys/class/hwmon/{h}/temp1_input")).unwrap_or(0);
        if milli > 0 {
            return Some(milli as f32 / 1000.0);
        }
    }
    // x86_pkg_temp from the thermal framework, for machines without hwmon
    for z in dir_entries("/sys/class/thermal") {
        if !z.starts_with("thermal_zone") {
            continue;
        }
        if read_first_line(&format!("/sys/class/thermal/{z}/type")).as_deref()
            == Some("x86_pkg_temp")
        {
            let milli = read_i64(&format!("/sys/class/thermal/{z}/temp")).unwrap_or(0);
            if milli > 0 {
                return Some(milli as f32 / 1000.0);
            }
        }
    }
    None
}

fn cpu_info() -> Option<CpuInfo> {
    #[cfg(target_os = "macos")]
    {
        let name = sysctl_str("machdep.cpu.brand_string")?;
        // Intel's brand string already carries the frequency:
        // "Intel(R) Core(TM) i5-3210M CPU @ 2.50GHz". hw.cpufrequency_max adds
        // it again, so cut it here. Linux does the same for /proc/cpuinfo's
        // model name.
        let name = match name.find(" @ ") {
            Some(at) => name[..at].trim_end().to_string(),
            None => name,
        };
        let freq_hz: i64 = sysctl_num("hw.cpufrequency_max");
        Some(CpuInfo {
            name,
            threads: sysctl_num("hw.logicalcpu"),
            cores: sysctl_num("hw.physicalcpu"),
            max_ghz: if freq_hz > 0 {
                freq_hz as f32 / 1e9
            } else {
                0.0
            },
            temp_c: None,
            profile: None,
        })
    }
    #[cfg(not(target_os = "macos"))]
    {
        let mut name = String::new();
        let mut threads = 0;
        // (physical id, core id) pairs; ARM has neither, and there each
        // logical CPU is a core.
        let mut core_ids: Vec<(i32, i32)> = Vec::new();
        let mut phys = 0;

        // Try the x86 model name first
        if let Some(text) = read_file("/proc/cpuinfo") {
            for line in text.split('\n') {
                let val = || {
                    line.find(':')
                        .map(|c| strip_eol(&line[c + 1..]).trim().to_string())
                        .unwrap_or_default()
                };
                if name.is_empty() && line.starts_with("model name") {
                    let v = val();
                    if !v.is_empty() {
                        name = match v.find(" @ ") {
                            Some(at) => v[..at].to_string(),
                            None => v,
                        };
                    }
                } else if line.starts_with("processor") {
                    threads += 1;
                } else if line.starts_with("physical id") {
                    phys = atoi(&val());
                } else if line.starts_with("core id") {
                    let id = (phys, atoi(&val()));
                    if !core_ids.contains(&id) {
                        core_ids.push(id);
                    }
                }
            }
        }

        // ARM/Apple Silicon: extract the chip from the device-tree model
        if name.is_empty() {
            if let Some(chip) = devicetree_chip() {
                name = format!("Apple {chip}");
            }
        }
        if name.is_empty() {
            return None;
        }

        // Max frequency from cpufreq
        let mut max_khz: i64 = 0;
        for policy in dir_entries("/sys/devices/system/cpu/cpufreq") {
            if !policy.starts_with("policy") {
                continue;
            }
            if let Some(khz) = read_i64(&format!(
                "/sys/devices/system/cpu/cpufreq/{policy}/cpuinfo_max_freq"
            )) {
                max_khz = max_khz.max(khz);
            }
        }

        let freq = "/sys/devices/system/cpu/cpu0/cpufreq";
        let profile = read_first_line(&format!("{freq}/energy_performance_preference"))
            .filter(|p| p != "default")
            .or_else(|| read_first_line(&format!("{freq}/scaling_governor")));

        Some(CpuInfo {
            name,
            threads,
            cores: if core_ids.is_empty() {
                threads
            } else {
                core_ids.len() as i32
            },
            max_ghz: max_khz as f32 / 1_000_000.0,
            temp_c: cpu_temp(),
            profile,
        })
    }
}

fn gather_cpu(info: &mut Info) {
    let Some(c) = cpu_info() else {
        return;
    };
    let name = &c.name;
    let n = c.threads;
    if n > 0 && c.max_ghz > 0.0 {
        info.add_info("CPU", &format!("{name} ({n}) @ {:.2} GHz", c.max_ghz));
    } else if n > 0 {
        info.add_info("CPU", &format!("{name} ({n})"));
    } else {
        info.add_info("CPU", name);
    }
}

/// extract the human product name from `lspci -d VVVV:DDDD` output.
/// "01:00.0 VGA compatible controller: NVIDIA Corporation AD106M
/// [GeForce RTX 4070 Max-Q / Mobile] (rev a1)". no corporation prefix
#[cfg(not(target_os = "macos"))]
fn gpu_lookup_lspci(pci_id: &str) -> Option<String> {
    if !is_pci_id(pci_id) {
        return None;
    }
    // a PCI ID names the same product forever, so this is cached for good
    // without lspci there's nothing to ask, and nothing to remember: the
    // name should show up once pciutils is installed
    if !have_bin("lspci") {
        return None;
    }
    let name = cache::get_or(&format!("pci:{pci_id}"), || {
        run_argv("lspci", &["-d", pci_id])
            .and_then(|out| lspci_product_name(&out))
            .unwrap_or_default()
    });
    (!name.is_empty()).then_some(name)
}

#[cfg(not(target_os = "macos"))]
fn lspci_product_name(out: &str) -> Option<String> {
    let mut line = strip_eol(out.split('\n').next()?).to_string();
    if line.is_empty() {
        return None;
    }
    if let Some(rev) = line.find(" (rev ") {
        line.truncate(rev);
    }

    // AMD's lspci strings always the vendor tag in brackets
    // "[AMD/ATI]"); discrete cards wrap the product name in a
    // second pair ("[Radeon RX 9070 XT]"), but some iGPUs only have the
    // single vendor bracket. Blindly taking the last bracket pair picks up
    // "AMD/ATI" in that case, so count brackets first.
    let bracket_count = line.bytes().filter(|&c| c == b'[').count();
    let lb = line.rfind('[');
    let rb = line.rfind(']');

    let name: Option<String> = match (lb, rb) {
        (Some(lb), Some(rb)) if rb > lb && bracket_count >= 2 => Some(line[lb + 1..rb].to_string()),
        (Some(_), Some(rb)) => {
            let after = line[rb + 1..].trim_start_matches(' ');
            if after.is_empty() {
                None
            } else {
                Some(after.to_string())
            }
        }
        _ => None,
    };

    let name = name.or_else(|| {
        let from = if let Some(c) = line.find(" Corporation ") {
            Some(c + 13)
        } else if let Some(c) = line.find(" Corp ") {
            Some(c + 6)
        } else {
            // Skip past the "bus:dev.fn class:" prefix even if no Corporation.
            line.find(':')
                .and_then(|c1| line[c1 + 1..].find(':').map(|c2| c1 + 1 + c2 + 1))
        };
        let s = match from {
            Some(i) => &line[i..],
            None => &line[..],
        };
        Some(s.trim_start_matches(' ').to_string())
    });

    name.filter(|n| !n.is_empty())
}

#[cfg(not(target_os = "macos"))]
fn amd_name_is_igpu(name: &str) -> bool {
    const CODENAMES: [&str; 16] = [
        "Raphael",
        "Phoenix",
        "Phoenix2",
        "Hawk Point",
        "Renoir",
        "Cezanne",
        "Rembrandt",
        "Picasso",
        "Raven",
        "Raven2",
        "Van Gogh",
        "Mendocino",
        "Barcelo",
        "Strix Point",
        "Strix Halo",
        "Krackan Point",
    ];
    CODENAMES.iter().any(|c| contains_ci(name, c))
}

struct Gpu {
    name: String,
    /// "Integrated", "Discrete" or ""
    kind: &'static str,
    /// "Intel", "AMD", "NVIDIA", or "" when the name already says it
    vendor: &'static str,
}

/// every GPU in the machine.
#[cfg(target_os = "macos")]
fn gpu_list() -> Vec<Gpu> {
    let Some(text) = sp_displays_text() else {
        return Vec::new();
    };
    // Both on dual-GPU MacBook Pros (Intel + discrete).
    parse_sp_displays(text)
        .0
        .into_iter()
        .map(|name| Gpu {
            name,
            kind: "",
            vendor: "",
        })
        .collect()
}

fn gather_gpu(info: &mut Info) {
    for g in gpu_list() {
        if g.kind.is_empty() {
            info.add_info("GPU", &g.name);
        } else {
            info.add_info("GPU", &format!("{} [{}]", g.name, g.kind));
        }
    }
}

#[cfg(not(target_os = "macos"))]
fn gpu_list() -> Vec<Gpu> {
    let mut gpus = Vec::new();
    for entry in dir_entries("/sys/class/drm") {
        let Some(rest) = entry.strip_prefix("card") else {
            continue;
        };
        if rest.is_empty() || !rest.bytes().all(|c| c.is_ascii_digit()) {
            continue;
        }

        let Some(uevent) = read_file(&format!("/sys/class/drm/{entry}/device/uevent")) else {
            continue;
        };
        let mut driver = String::new();
        let mut pci_id = String::new();
        let mut compat = String::new();
        for line in uevent.split('\n') {
            let line = strip_eol(line);
            if let Some(v) = line.strip_prefix("DRIVER=") {
                driver = v.to_string();
            } else if let Some(v) = line.strip_prefix("PCI_ID=") {
                pci_id = v.to_string();
            } else if let Some(v) = line.strip_prefix("OF_COMPATIBLE_0=") {
                compat = v.to_string();
            }
        }

        // Skip display subsystems — they're not GPUs
        if compat.contains("display-subsystem") {
            continue;
        }

        let mut name = String::new();
        let mut kind = "";

        if compat.starts_with("apple,agx") {
            name = match devicetree_chip() {
                Some(chip) => format!("Apple {chip}"),
                None => "Apple GPU".to_string(),
            };
            kind = "Integrated";
        } else if let Some(n) = gpu_lookup_lspci(&pci_id) {
            // lspci gave us a human name.
            name = n;
        } else if driver == "i915" || driver == "xe" {
            name = "Intel Graphics".to_string();
        } else if driver == "amdgpu" || driver == "radeon" {
            name = "AMD Graphics".to_string();
        } else if driver == "nvidia" || driver == "nouveau" {
            name = "NVIDIA GPU".to_string();
        } else if !driver.is_empty() {
            name = driver.clone();
        }

        if kind.is_empty() {
            kind = if driver == "i915" || driver == "xe" {
                "Integrated"
            } else if driver == "nvidia" || driver == "nouveau" {
                "Discrete"
            } else if driver == "amdgpu" {
                if amd_name_is_igpu(&name) {
                    "Integrated"
                } else {
                    "Discrete"
                }
            } else {
                ""
            };
        }

        if name.is_empty() {
            continue;
        }
        let vendor = match driver.as_str() {
            "i915" | "xe" => "Intel",
            "amdgpu" | "radeon" => "AMD",
            "nvidia" | "nouveau" => "NVIDIA",
            _ => "",
        };
        gpus.push(Gpu { name, kind, vendor });
    }
    gpus
}

/// What the graphics stack offers the default GPU: ["OpenGL 4.6", "Vulkan
/// 1.3", "VA-API"]. Only the tools that ship with Mesa and libva can say
/// (every Mesa install carries the Vulkan ICDs of every driver, so the files
/// alone don't), and they take a few hundred milliseconds, so the answer is
/// cached under the driver install it came from.
#[cfg(not(target_os = "macos"))]
fn gpu_apis() -> Vec<String> {
    // NixOS keeps the whole stack behind one symlink; elsewhere, a driver
    // upgrade rewrites these directories
    let stamp = {
        use std::os::unix::fs::MetadataExt;
        let mut parts: Vec<String> = Vec::new();
        if let Ok(p) = fs::canonicalize("/run/opengl-driver") {
            parts.push(p.to_string_lossy().into_owned());
        }
        for d in [
            "/usr/lib/dri",
            "/usr/lib64/dri",
            "/usr/lib/x86_64-linux-gnu/dri",
            "/usr/lib/aarch64-linux-gnu/dri",
            "/usr/share/vulkan/icd.d",
        ] {
            if let Ok(m) = fs::metadata(d) {
                parts.push(format!("{d}@{}", m.mtime()));
            }
        }
        for t in ["glxinfo", "eglinfo", "vulkaninfo", "vainfo"] {
            if which(t).is_some() {
                parts.push(t.to_string());
            }
        }
        parts.join(",")
    };
    let val = cache::get_or(&format!("gfx:{stamp}"), || {
        // first token after the colon of the first line starting with a key
        let field = |text: &str, keys: &[&str]| -> Option<String> {
            keys.iter().find_map(|k| {
                text.lines()
                    .map(str::trim_start)
                    .find(|l| l.starts_with(k))
                    .and_then(|l| l.split_once(':'))
                    .and_then(|(_, v)| v.split_whitespace().next())
                    .map(str::to_string)
            })
        };
        let gl = || {
            let text = have_bin("glxinfo")
                .then(|| run_argv("glxinfo", &["-B"]))
                .flatten()
                .filter(|t| t.contains("OpenGL"))
                .or_else(|| {
                    have_bin("eglinfo")
                        .then(|| run_argv("eglinfo", &["-B"]))
                        .flatten()
                })?;
            field(
                &text,
                &["OpenGL core profile version", "OpenGL version string"],
            )
            .map(|v| format!("OpenGL {v}"))
        };
        let vk = || {
            let text = have_bin("vulkaninfo")
                .then(|| run_argv("vulkaninfo", &["--summary"]))
                .flatten()?;
            // the first real device: skip llvmpipe and other CPU renderers
            let mut api = None;
            for line in text.lines().map(str::trim_start) {
                if line.starts_with("apiVersion") {
                    api = line.split_once('=').map(|(_, v)| v.trim().to_string());
                } else if line.starts_with("deviceType") {
                    if !line.contains("CPU") {
                        break;
                    }
                    api = None;
                }
            }
            // "1.3.352" -> "Vulkan 1.3"
            let api = api?;
            let v: Vec<&str> = api.splitn(3, '.').take(2).collect();
            Some(format!("Vulkan {}", v.join(".")))
        };
        let va = || {
            let text = have_bin("vainfo")
                .then(|| run_argv("vainfo", &[]))
                .flatten()?;
            text.contains("Driver version")
                .then(|| "VA-API".to_string())
        };
        let found: Vec<String> = std::thread::scope(|s| {
            let jobs: [&(dyn Fn() -> Option<String> + Sync); 3] = [&gl, &vk, &va];
            let handles: Vec<_> = jobs
                .iter()
                .map(|&j| {
                    std::thread::Builder::new()
                        .spawn_scoped(s, j)
                        .map_err(|_| j)
                })
                .collect();
            handles
                .into_iter()
                .filter_map(|h| match h {
                    Ok(h) => h.join().ok().flatten(),
                    Err(j) => j(),
                })
                .collect()
        });
        found.join("|")
    });
    val.split('|')
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

#[cfg(target_os = "macos")]
fn gpu_apis() -> Vec<String> {
    vec!["Metal".to_string()]
}

/// Value of a `Key:   1234 kB` line in /proc/meminfo.
#[cfg(not(target_os = "macos"))]
fn meminfo_kb(text: &str, key: &str) -> i64 {
    for line in text.split('\n') {
        if let Some(v) = line.strip_prefix(key) {
            return v
                .split_ascii_whitespace()
                .next()
                .and_then(|n| n.parse::<i64>().ok())
                .unwrap_or(0);
        }
    }
    0
}

// --- macOS memory stats (Mach) ---
//
// `vm_stat` is a subprocess: every call means fork + exec + shell. The animation
// loop refreshes memory once a second, which made that a visible stutter on slow
// machines. host_statistics64 returns the same numbers through a syscall.
#[cfg(target_os = "macos")]
mod mach {
    pub type MachPort = u32;

    extern "C" {
        pub fn mach_host_self() -> MachPort;
        pub fn mach_task_self() -> MachPort;
        pub fn mach_port_deallocate(task: MachPort, name: MachPort) -> i32;
        pub fn host_statistics64(
            host: MachPort,
            flavor: i32,
            info: *mut u32,
            count: *mut u32,
        ) -> i32;
    }

    pub const HOST_VM_INFO64: i32 = 4;
    /// sizeof(vm_statistics64_data_t) / sizeof(integer_t)
    pub const HOST_VM_INFO64_COUNT: u32 = 38;

    // vm_statistics64 field offsets (as u32 indices). The first four fields are
    // natural_t, then 8-aligned uint64_t's follow with no padding.
    pub const IDX_FREE: usize = 0;
    pub const IDX_INACTIVE: usize = 2;
    pub const IDX_SPECULATIVE: usize = 23;
}

/// (free, inactive, speculative) page counts.
#[cfg(target_os = "macos")]
fn mach_vm_pages() -> Option<(i64, i64, i64)> {
    unsafe {
        let mut buf = [0u32; mach::HOST_VM_INFO64_COUNT as usize];
        let mut count = mach::HOST_VM_INFO64_COUNT;
        let host = mach::mach_host_self();
        let rc = mach::host_statistics64(host, mach::HOST_VM_INFO64, buf.as_mut_ptr(), &mut count);
        mach::mach_port_deallocate(mach::mach_task_self(), host);
        if rc != 0 || count < mach::HOST_VM_INFO64_COUNT {
            return None;
        }
        Some((
            buf[mach::IDX_FREE] as i64,
            buf[mach::IDX_INACTIVE] as i64,
            buf[mach::IDX_SPECULATIVE] as i64,
        ))
    }
}

/// `vm_stat` parsing. The fallback for when the Mach path doesn't work out.
#[cfg(target_os = "macos")]
fn vm_stat_pages() -> Option<(i64, i64, i64, i64)> {
    let text = run("vm_stat 2>/dev/null")?;
    let mut page_size: i64 = 4096;
    let (mut free, mut inactive, mut speculative) = (0i64, 0i64, 0i64);
    for line in text.lines() {
        let num = |l: &str| -> i64 {
            l.rsplit(':')
                .next()
                .unwrap_or("")
                .trim()
                .trim_end_matches('.')
                .parse()
                .unwrap_or(0)
        };
        if line.starts_with("Pages free:") {
            free = num(line);
        } else if line.starts_with("Pages inactive:") {
            inactive = num(line);
        } else if line.starts_with("Pages speculative:") {
            speculative = num(line);
        } else if let Some(p) = line.find("page size of") {
            page_size = line[p + 12..]
                .split_ascii_whitespace()
                .next()
                .and_then(|n| n.trim_end_matches(')').parse().ok())
                .unwrap_or(4096);
        }
    }
    Some((free, inactive, speculative, page_size))
}

/// (total, available) bytes. The fallback where there is no /proc.
#[cfg(not(target_os = "macos"))]
fn sysinfo_mem() -> Option<(i64, i64)> {
    let mut si: libc::sysinfo = unsafe { std::mem::zeroed() };
    if unsafe { libc::sysinfo(&mut si) } != 0 {
        return None;
    }
    let unit = if si.mem_unit == 0 {
        1i64
    } else {
        si.mem_unit as i64
    };
    let total = si.totalram as i64 * unit;
    // sysinfo has no MemAvailable; free + buffers is the closest thing.
    let avail = (si.freeram as i64 + si.bufferram as i64) * unit;
    if total <= 0 {
        return None;
    }
    Some((total, avail.clamp(0, total)))
}

/// (used, total) bytes of RAM.
fn mem_usage() -> Option<(i64, i64)> {
    #[cfg(target_os = "macos")]
    {
        let total: i64 = sysctl_num("hw.memsize");
        if total <= 0 {
            return None;
        }
        // hw.pagesize is 64-bit on current XNU and 16K on Apple Silicon;
        // reading it into 32 bits failed and fell back to 4K
        let page_size = match unsafe { libc::sysconf(libc::_SC_PAGESIZE) } {
            n if n > 0 => n as i64,
            _ => 4096,
        };

        let avail = mach_vm_pages()
            .map(|(f, i, s)| (f + i + s) * page_size)
            // Wrong field indices give a nonsense number; fall back to vm_stat.
            .filter(|&a| a > 0 && a <= total)
            .or_else(|| vm_stat_pages().map(|(f, i, s, ps)| (f + i + s) * ps))
            .unwrap_or(0);
        Some((total - avail, total))
    }
    #[cfg(not(target_os = "macos"))]
    {
        // iSH has no /proc/meminfo at all; carry on with the sysinfo(2) syscall.
        let Some(text) = read_file("/proc/meminfo") else {
            return sysinfo_mem().map(|(total, avail)| (total - avail, total));
        };
        let total = meminfo_kb(&text, "MemTotal:");
        if total <= 0 {
            return None;
        }
        // MemAvailable has existed since 3.14, but synthetic /proc's like iSH's
        // don't provide it, and reading 0 made memory look 100% used. This is
        // the same estimate older kernels used: free + reclaimable.
        let mut avail = meminfo_kb(&text, "MemAvailable:");
        if avail <= 0 {
            let free = meminfo_kb(&text, "MemFree:");
            let cached = meminfo_kb(&text, "Cached:");
            let buffers = meminfo_kb(&text, "Buffers:");
            let reclaimable = meminfo_kb(&text, "SReclaimable:");
            let shmem = meminfo_kb(&text, "Shmem:");
            avail = (free + cached + buffers + reclaimable - shmem).max(free);
        }
        let avail = avail.clamp(0, total);
        Some(((total - avail) * 1024, total * 1024))
    }
}

fn gather_memory(info: &mut Info) {
    let Some((used, total)) = mem_usage() else {
        return;
    };
    let pct = used * 100 / total;
    info.add_info(
        "Memory",
        &format!(
            "{:.2} GiB / {:.2} GiB (\x1b[{}m{pct}%\x1b[0m)",
            used as f64 / GIB,
            total as f64 / GIB,
            pct_color(pct)
        ),
    );
}

/// (used, total) bytes of swap, None when there is none.
fn swap_usage() -> Option<(i64, i64)> {
    #[cfg(target_os = "macos")]
    {
        // "total = 2048.00M  used = 512.00M  free = 1536.00M  (encrypted)"
        let s = sysctl_str("vm.swapusage")?;
        let mb = |key: &str| -> f64 {
            s.find(key)
                .map(|i| atof(s[i + key.len()..].trim_start()) as f64)
                .unwrap_or(0.0)
        };
        let total = (mb("total = ") * 1048576.0) as i64;
        let used = (mb("used = ") * 1048576.0) as i64;
        (total > 0).then_some((used, total))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let text = read_file("/proc/meminfo")?;
        let total = meminfo_kb(&text, "SwapTotal:");
        let free = meminfo_kb(&text, "SwapFree:");
        (total > 0).then_some(((total - free) * 1024, total * 1024))
    }
}

fn gather_swap(info: &mut Info) {
    let Some((used, total)) = swap_usage() else {
        return;
    };
    let pct = used * 100 / total;
    let color = pct_color(pct);
    let (div, unit) = if total >= 1 << 30 {
        (GIB, "GiB")
    } else {
        (1048576.0, "MiB")
    };
    info.add_info(
        "Swap",
        &format!(
            "{:.2} {unit} / {:.2} {unit} (\x1b[{color}m{pct}%\x1b[0m)",
            used as f64 / div,
            total as f64 / div
        ),
    );
}

#[cfg(target_os = "macos")]
fn fstype_of(path: &str) -> String {
    unsafe {
        let mut mnts: *mut libc::statfs = std::ptr::null_mut();
        let count = libc::getmntinfo(&mut mnts, libc::MNT_NOWAIT);
        for i in 0..count {
            let m = &*mnts.offset(i as isize);
            if CStr::from_ptr(m.f_mntonname.as_ptr()).to_string_lossy() == path {
                return CStr::from_ptr(m.f_fstypename.as_ptr())
                    .to_string_lossy()
                    .into_owned();
            }
        }
    }
    String::new()
}

#[cfg(not(target_os = "macos"))]
fn fstype_of(path: &str) -> String {
    let Some(text) = read_file("/proc/mounts") else {
        return String::new();
    };
    // The last entry wins: an automount point is listed once as autofs and
    // again, below it, as what actually got mounted there.
    let mut found = String::new();
    for line in text.lines() {
        let mut it = line.split_ascii_whitespace();
        let (_dev, mnt, fs) = (it.next(), it.next(), it.next());
        if mnt == Some(path) {
            found = fs.unwrap_or("").to_string();
        }
    }
    found
}

/// (used, total) bytes of the filesystem under `path`.
fn disk_usage(path: &str) -> Option<(f64, f64)> {
    let st = statvfs_of(path)?;
    if st.f_blocks == 0 {
        return None;
    }
    let total = st.f_blocks as f64 * st.f_frsize as f64;
    let free = st.f_bfree as f64 * st.f_frsize as f64;
    Some((total - free, total))
}

fn gather_disk_one(info: &mut Info, path: &str) {
    let Some((used, total)) = disk_usage(path) else {
        return;
    };
    let pct = (used * 100.0 / total) as i64;
    let color = pct_color(pct);
    let (used_gib, total_gib) = (used / GIB, total / GIB);

    let label = format!("Disk ({path})");
    let fstype = fstype_of(path);
    if fstype.is_empty() {
        info.add_info(
            &label,
            &format!("{used_gib:.2} GiB / {total_gib:.2} GiB (\x1b[{color}m{pct}%\x1b[0m)"),
        );
    } else {
        info.add_info(
            &label,
            &format!(
                "{used_gib:.2} GiB / {total_gib:.2} GiB (\x1b[{color}m{pct}%\x1b[0m) - {fstype}"
            ),
        );
    }
}

fn gather_disk(info: &mut Info, cfg: &Config) {
    gather_disk_one(info, "/");
    for d in &cfg.extra_disks {
        gather_disk_one(info, d);
    }
}

/// Some ACPI batteries report model_name as hex ASCII:
/// "0x4B384234314341" -> "K8B41CA". None keeps the raw value.
#[cfg(not(target_os = "macos"))]
fn decode_hex_ascii(s: &str) -> Option<String> {
    let body = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X"))?;
    if body.is_empty() || body.len() % 2 != 0 || !body.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let mut out = String::with_capacity(body.len() / 2);
    for pair in body.as_bytes().chunks(2) {
        let hi = (pair[0] as char).to_digit(16)? as u8;
        let lo = (pair[1] as char).to_digit(16)? as u8;
        let c = (hi << 4) | lo;
        // Not printable ASCII means the decode is meaningless; keep the raw value.
        if !(0x20..0x7f).contains(&c) {
            return None;
        }
        out.push(c as char);
    }
    let out = out.trim().to_string();
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

fn battery_color(capacity: i64) -> &'static str {
    if capacity >= 50 {
        "32"
    } else if capacity >= 20 {
        "93"
    } else {
        "31"
    }
}

fn time_remaining(mins_left: i64) -> String {
    let hours = mins_left / 60;
    let mins = mins_left % 60;
    let s = |n: i64| if n == 1 { "" } else { "s" };
    if hours > 0 {
        format!(
            "{} hour{}, {} min{} remaining",
            hours,
            s(hours),
            mins,
            s(mins)
        )
    } else {
        format!("{} min{} remaining", mins, s(mins))
    }
}

struct Battery {
    /// percent
    capacity: i64,
    /// "Charging", "Discharging", "Full", ... as the kernel spells it
    status: String,
    /// minutes until empty while discharging, until full while charging
    mins_left: Option<i64>,
    /// current draw in watts, when the battery reports it
    watts: Option<f32>,
    model: Option<String>,
}

#[cfg(target_os = "macos")]
fn battery() -> Option<Battery> {
    let (capacity, is_charging, time_to_empty) = mac_battery()?;
    if capacity < 0 {
        return None;
    }
    let status = if is_charging {
        "Charging"
    } else if time_to_empty >= 0 {
        "Discharging"
    } else {
        ""
    };
    Some(Battery {
        capacity,
        status: status.to_string(),
        mins_left: (time_to_empty > 0 && !is_charging).then_some(time_to_empty),
        watts: None,
        model: None,
    })
}

#[cfg(not(target_os = "macos"))]
fn battery() -> Option<Battery> {
    // The first battery in /sys/class/power_supply. Peripherals (mice,
    // headsets) report a capacity too, but say scope=Device.
    let bat = dir_entries("/sys/class/power_supply")
        .into_iter()
        .find(|n| {
            let d = format!("/sys/class/power_supply/{n}");
            fs::metadata(format!("{d}/capacity")).is_ok()
                && read_first_line(&format!("{d}/scope")).as_deref() != Some("Device")
        })?;
    let attr = |k: &str| format!("/sys/class/power_supply/{bat}/{k}");

    let mut capacity: i64 = -1;

    // Prefer energy_now/energy_full for an accurate percentage
    let energy_now = read_i64(&attr("energy_now")).unwrap_or(0);
    let energy_full = read_i64(&attr("energy_full")).unwrap_or(0);
    if energy_full > 0 {
        capacity = energy_now * 100 / energy_full;
    }

    // Fall back to charge_now/charge_full
    let charge_now = read_i64(&attr("charge_now")).unwrap_or(0);
    let charge_full = read_i64(&attr("charge_full")).unwrap_or(0);
    if capacity < 0 && charge_full > 0 {
        capacity = charge_now * 100 / charge_full;
    }

    // Last resort: the capacity file
    if capacity < 0 {
        capacity = read_i64(&attr("capacity")).unwrap_or(-1);
    }
    if capacity < 0 {
        return None;
    }
    let capacity = capacity.min(100);

    let status = read_first_line(&attr("status")).unwrap_or_default();

    // power_now is negative when discharging on some systems. Batteries that
    // count charge instead of energy give current and voltage.
    let power_uw = match read_i64(&attr("power_now")) {
        Some(p) => p.abs(),
        None => {
            let ua = read_i64(&attr("current_now")).unwrap_or(0).abs();
            let uv = read_i64(&attr("voltage_now")).unwrap_or(0).abs();
            ua * (uv / 1000) / 1000
        }
    };
    let watts = (power_uw > 0).then_some(power_uw as f32 / 1e6);

    // Time from the same units the battery counts in.
    let mins_left = if power_uw > 0 && energy_full > 0 {
        let rate = power_uw as f32;
        match status.as_str() {
            "Discharging" => Some((energy_now as f32 / rate * 60.0) as i64),
            "Charging" => Some(((energy_full - energy_now).max(0) as f32 / rate * 60.0) as i64),
            _ => None,
        }
    } else if charge_full > 0 {
        let ua = read_i64(&attr("current_now")).unwrap_or(0).abs() as f32;
        match status.as_str() {
            "Discharging" if ua > 0.0 => Some((charge_now as f32 / ua * 60.0) as i64),
            "Charging" if ua > 0.0 => {
                Some(((charge_full - charge_now).max(0) as f32 / ua * 60.0) as i64)
            }
            _ => None,
        }
    } else {
        None
    };

    let model = read_first_line(&attr("model_name"))
        .or_else(|| read_first_line(&attr("manufacturer")))
        .map(|m| m.trim_end_matches([' ', '\r', '\n']).to_string())
        .map(|m| decode_hex_ascii(&m).unwrap_or(m))
        .filter(|m| !m.is_empty());

    Some(Battery {
        capacity,
        status,
        mins_left,
        watts,
        model,
    })
}

fn gather_battery(info: &mut Info) {
    let Some(b) = battery() else {
        return;
    };
    let label = match &b.model {
        Some(m) => format!("Battery ({m})"),
        None => "Battery".to_string(),
    };
    let color = battery_color(b.capacity);
    let capacity = b.capacity;
    let time_str = match b.mins_left {
        Some(m) if b.status == "Discharging" => time_remaining(m),
        _ => String::new(),
    };
    let status = &b.status;
    if !time_str.is_empty() && !status.is_empty() {
        info.add_info(
            &label,
            &format!("\x1b[{color}m{capacity}%\x1b[0m ({time_str}) [{status}]"),
        );
    } else if !status.is_empty() {
        info.add_info(
            &label,
            &format!("\x1b[{color}m{capacity}%\x1b[0m [{status}]"),
        );
    } else {
        info.add_info(&label, &format!("\x1b[{color}m{capacity}%\x1b[0m"));
    }
}

/// (name, version) of the terminal emulator. Termux reports its name and
/// version as one string, in `name`.
fn terminal_info() -> Option<(String, String)> {
    let mut term = String::new();

    if let Some(t) = mobile::terminal() {
        return Some((t, String::new()));
    }

    // Try TERM_PROGRAM first, then the well-known per-terminal env vars
    if let Ok(tp) = std::env::var("TERM_PROGRAM") {
        if !tp.is_empty() {
            term = tp;
        }
    }
    if term.is_empty() {
        if std::env::var_os("KITTY_WINDOW_ID").is_some() {
            term = "kitty".to_string();
        } else if std::env::var_os("ALACRITTY_LOG").is_some() {
            term = "alacritty".to_string();
        } else if std::env::var_os("WEZTERM_PANE").is_some() {
            term = "wezterm".to_string();
        } else if std::env::var_os("GHOSTTY_RESOURCES_DIR").is_some() {
            term = "ghostty".to_string();
        } else if let Some(te) = std::env::var_os("TERMINAL_EMULATOR") {
            term = te.to_string_lossy().into_owned();
        } else {
            // Walk up the process tree looking for the first non-shell
            let mut pid = std::process::id() as i32;
            for _ in 0..4 {
                let Some(ppid) = proc_ppid(pid) else { break };
                pid = ppid;
                let Some(comm) = proc_comm(pid) else { break };
                if !comm.is_empty() && !KNOWN_SHELLS.contains(&comm.as_str()) {
                    term = comm;
                    break;
                }
            }
        }
    }

    if term.is_empty() {
        return None;
    }
    let version = cmd_version(&term);
    Some((term, version))
}

fn gather_terminal(info: &mut Info) {
    let Some((term, version)) = terminal_info() else {
        return;
    };
    if version.is_empty() {
        info.add_info("Terminal", &term);
    } else {
        info.add_info("Terminal", &format!("{term} {version}"));
    }
}

#[cfg(not(target_os = "macos"))]
fn iface_is_wireless(name: &str) -> bool {
    fs::metadata(format!("/sys/class/net/{name}/wireless")).is_ok()
}

/// `networksetup -listallhardwareports` prints "Hardware Port: Wi-Fi" followed
/// by "Device: en0" for every port. Run it once and cache the interface -> port
/// name mapping.
#[cfg(target_os = "macos")]
fn mac_iface_port(name: &str) -> Option<&'static str> {
    static CACHE: std::sync::OnceLock<Vec<(String, String)>> = std::sync::OnceLock::new();
    let map = CACHE.get_or_init(|| {
        let mut v = Vec::new();
        let Some(text) = run("networksetup -listallhardwareports 2>/dev/null") else {
            return v;
        };
        let mut port: Option<String> = None;
        for line in text.lines() {
            let line = line.trim();
            if let Some(p) = line.strip_prefix("Hardware Port:") {
                port = Some(p.trim().to_string());
            } else if let Some(d) = line.strip_prefix("Device:") {
                if let Some(p) = port.take() {
                    v.push((d.trim().to_string(), p));
                }
            }
        }
        v
    });

    let port = map.iter().find(|(dev, _)| dev == name).map(|(_, p)| p)?;
    // Apple's port names change over time ("Wi-Fi", "AirPort" on older releases).
    if port.eq_ignore_ascii_case("Wi-Fi") || port.eq_ignore_ascii_case("AirPort") {
        Some("Wi-Fi")
    } else if port.contains("Ethernet") || port.contains("LAN") {
        Some("Ethernet")
    } else if port.contains("Thunderbolt") {
        Some("Thunderbolt")
    } else if port.contains("Bluetooth") {
        Some("Bluetooth")
    } else {
        None
    }
}

#[cfg(target_os = "macos")]
fn iface_kind_platform(name: &str, is_default: bool) -> Option<&'static str> {
    if let Some(kind) = mac_iface_port(name) {
        return Some(kind);
    }
    // Old behaviour, for no networksetup or an interface missing from its list.
    if name.starts_with("en") && is_default {
        Some("Ethernet")
    } else {
        None
    }
}

#[cfg(not(target_os = "macos"))]
fn iface_kind_platform(name: &str, _is_default: bool) -> Option<&'static str> {
    if iface_is_wireless(name) {
        Some("Wi-Fi")
    } else if name.starts_with("eth") || name.starts_with("en") {
        Some("Ethernet")
    } else {
        None
    }
}

/// best-effort human label for an interface. Falls back to the raw name
/// "Local IP (...)" for anything unrecognized.
fn iface_label(name: &str, is_default: bool) -> String {
    let kind: Option<&str> = if name.starts_with("tailscale") {
        Some("Tailscale")
    } else if name.starts_with("zt") {
        Some("ZeroTier")
    } else if name.starts_with("wg") {
        Some("WireGuard")
    } else if name.starts_with("ppp") {
        Some("PPP")
    } else if name.starts_with("docker")
        || name.starts_with("br-")
        || name.starts_with("veth")
        || name.starts_with("virbr")
    {
        Some("Virtual")
    } else if name.starts_with("utun") || name.starts_with("tun") || name.starts_with("tap") {
        Some("VPN")
    } else {
        iface_kind_platform(name, is_default)
    };

    match kind {
        Some(k) => format!("{k} ({name})"),
        None => format!("Local IP ({name})"),
    }
}

#[cfg(target_os = "macos")]
fn get_default_route_iface() -> Option<String> {
    let out = run("route -n get default 2>/dev/null")?;
    for line in out.lines() {
        if let Some(p) = line.find("interface:") {
            let v = line[p + 10..].trim();
            if !v.is_empty() {
                return Some(v.to_string());
            }
            break;
        }
    }
    None
}

#[cfg(not(target_os = "macos"))]
fn get_default_route_iface() -> Option<String> {
    let text = read_file("/proc/net/route")?;
    let mut best: Option<(u64, String)> = None;
    for line in text.lines().skip(1) {
        let f: Vec<&str> = line.split_ascii_whitespace().collect();
        if f.len() < 7 {
            continue;
        }
        let Ok(dest) = u64::from_str_radix(f[1], 16) else {
            continue;
        };
        let Ok(metric) = f[6].parse::<u64>() else {
            continue;
        };
        if dest == 0 && best.as_ref().map_or(true, |(m, _)| metric < *m) {
            best = Some((metric, f[0].to_string()));
        }
    }
    best.map(|(_, iface)| iface)
}

/// IPv4 addresses as (interface, address, prefix bits, carries the default
/// route), loopback left out.
fn ipv4_list() -> Vec<(String, String, u32, bool)> {
    let default_iface = get_default_route_iface().unwrap_or_default();
    let loopback = if cfg!(target_os = "macos") {
        "lo0"
    } else {
        "lo"
    };

    let mut entries: Vec<(String, String, u32, bool)> = Vec::new();
    unsafe {
        let mut list: *mut libc::ifaddrs = std::ptr::null_mut();
        if libc::getifaddrs(&mut list) != 0 {
            return entries;
        }
        let mut cur = list;
        while !cur.is_null() && entries.len() < 16 {
            let ifa = &*cur;
            cur = ifa.ifa_next;
            if ifa.ifa_addr.is_null() || (*ifa.ifa_addr).sa_family as i32 != libc::AF_INET {
                continue;
            }
            let name = CStr::from_ptr(ifa.ifa_name).to_string_lossy().into_owned();
            if name == loopback {
                continue;
            }

            let sa = ifa.ifa_addr as *const libc::sockaddr_in;
            let octets = (*sa).sin_addr.s_addr.to_ne_bytes();
            let addr = format!("{}.{}.{}.{}", octets[0], octets[1], octets[2], octets[3]);

            let mut bits = 0u32;
            if !ifa.ifa_netmask.is_null() {
                let mask = ifa.ifa_netmask as *const libc::sockaddr_in;
                let mut m = u32::from_be((*mask).sin_addr.s_addr);
                while m & 0x8000_0000 != 0 {
                    bits += 1;
                    m <<= 1;
                }
            }

            let is_default = !default_iface.is_empty() && name == default_iface;
            entries.push((name, addr, bits, is_default));
        }
        libc::freeifaddrs(list);
    }
    // Default route first, everything else after.
    entries.sort_by_key(|e| !e.3);
    entries
}

fn gather_ip(info: &mut Info) {
    for (name, addr, bits, is_def) in ipv4_list() {
        let shown = if bits > 0 {
            format!("{addr}/{bits}")
        } else {
            addr
        };
        info.add_info(&iface_label(&name, is_def), &shown);
    }
}

/// sound devices by name, HDMI/DisplayPort outputs left out. HDA cards are
/// named after their codec ("Realtek ALC287"), which says more than the
/// controller ("HDA Intel PCH") does.
#[cfg(not(target_os = "macos"))]
fn audio_devices() -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let Some(cards) = read_file("/proc/asound/cards") else {
        return out;
    };
    let is_hdmi = |n: &str| contains_ci(n, "HDMI") || contains_ci(n, "DisplayPort");
    for line in cards.lines() {
        // " 0 [PCH            ]: HDA-Intel - HDA Intel PCH"
        let t = line.trim_start();
        let Some(num) = t
            .split(' ')
            .next()
            .filter(|n| n.bytes().all(|c| c.is_ascii_digit()))
        else {
            continue;
        };
        if num.is_empty() {
            continue;
        }
        let dir = format!("/proc/asound/card{num}");
        let mut codecs: Vec<String> = dir_entries(&dir)
            .into_iter()
            .filter(|e| e.starts_with("codec#"))
            .filter_map(|e| read_first_line(&format!("{dir}/{e}")))
            .filter_map(|l| l.strip_prefix("Codec: ").map(str::to_string))
            .collect();
        codecs.sort();
        let names: Vec<String> = if codecs.is_empty() {
            t.split_once(" - ")
                .map(|(_, n)| vec![n.trim().to_string()])
                .unwrap_or_default()
        } else {
            codecs
        };
        for n in names {
            if !n.is_empty() && !is_hdmi(&n) && !out.contains(&n) {
                out.push(n);
            }
        }
    }
    out
}

#[cfg(target_os = "macos")]
fn audio_devices() -> Vec<String> {
    Vec::new()
}

/// default output volume in percent and whether it is muted, from
/// PipeWire's wpctl or PulseAudio's pactl.
fn audio_volume() -> Option<(i64, bool)> {
    if have_bin("wpctl") {
        // "Volume: 0.40" or "Volume: 0.40 [MUTED]"
        let o = run_argv("wpctl", &["get-volume", "@DEFAULT_AUDIO_SINK@"])?;
        let v = o.strip_prefix("Volume:")?.trim();
        let vol = (atof(v) * 100.0).round() as i64;
        return Some((vol, o.contains("MUTED")));
    }
    if have_bin("pactl") {
        // "Volume: front-left: 26214 /  40% / -23.88 dB,   front-right: ..."
        let o = run_argv("pactl", &["get-sink-volume", "@DEFAULT_SINK@"])?;
        let pct = o.find('%')?;
        let start = o[..pct].rfind(|c: char| !c.is_ascii_digit())? + 1;
        let vol = o[start..pct].parse().ok()?;
        let muted = run_argv("pactl", &["get-sink-mute", "@DEFAULT_SINK@"])
            .is_some_and(|m| m.contains("yes"));
        return Some((vol, muted));
    }
    None
}

fn gather_audio(info: &mut Info) {
    let devs = audio_devices();
    if devs.is_empty() {
        return;
    }
    let vol = match audio_volume() {
        Some((_, true)) => " (muted)".to_string(),
        Some((v, false)) => format!(" ({v}%)"),
        None => String::new(),
    };
    info.add_info("Audio", &format!("{}{vol}", devs.join(", ")));
}

fn gather_locale(info: &mut Info) {
    if let Ok(lang) = std::env::var("LANG") {
        if !lang.is_empty() {
            info.add_info("Locale", &lang);
        }
    }
}

// customizaton for theme - icons - font - cursor ---

/// read `key = value` out of an INI-ish file, optionally inside [section].
/// handles the GTK, Qt and KDE config dialects all four gathers need.
#[cfg(not(target_os = "macos"))]
fn read_ini_key(path: &str, section: Option<&str>, key: &str) -> Option<String> {
    let text = read_file(path)?;
    let mut in_section = section.is_none();

    for raw in text.split('\n') {
        let line = raw
            .trim_start_matches([' ', '\t'])
            .trim_end_matches(['\n', '\r', ' ', '\t']);

        if let Some(rest) = line.strip_prefix('[') {
            if let Some(sec) = section {
                in_section = rest
                    .strip_prefix(sec)
                    .map(|r| r.starts_with(']'))
                    .unwrap_or(false);
            }
            continue;
        }
        if !in_section {
            continue;
        }
        let Some(after_key) = line.strip_prefix(key) else {
            continue;
        };
        let Some(val) = after_key.trim_start_matches([' ', '\t']).strip_prefix('=') else {
            continue;
        };
        let mut val = val.trim_start_matches([' ', '\t']);
        let b = val.as_bytes();
        if b.len() >= 2
            && ((b[0] == b'"' && b[b.len() - 1] == b'"')
                || (b[0] == b'\'' && b[b.len() - 1] == b'\''))
        {
            val = &val[1..val.len() - 1];
        }
        return if val.is_empty() {
            None
        } else {
            Some(val.to_string())
        };
    }
    None
}

#[cfg(not(target_os = "macos"))]
fn read_gtk3_setting(key: &str) -> Option<String> {
    read_ini_key(
        &format!("{}/gtk-3.0/settings.ini", xdg_config_home()?),
        None,
        key,
    )
}

#[cfg(not(target_os = "macos"))]
fn read_gtk2_setting(key: &str) -> Option<String> {
    if let Some(home) = home_dir() {
        if let Some(v) = read_ini_key(&format!("{home}/.gtkrc-2.0"), None, key) {
            return Some(v);
        }
    }
    read_ini_key(&format!("{}/gtk-2.0/gtkrc", xdg_config_home()?), None, key)
}

/// qt6ct/qt5ct config path, preferring whatever QT_QPA_PLATFORMTHEME names.
#[cfg(not(target_os = "macos"))]
fn qtct_conf_path() -> Option<String> {
    let base = xdg_config_home()?;
    if let Ok(platform) = std::env::var("QT_QPA_PLATFORMTHEME") {
        if platform.starts_with("qt") {
            let p = format!("{base}/{platform}/{platform}.conf");
            if fs::metadata(&p).is_ok() {
                return Some(p);
            }
        }
    }
    for variant in ["qt6ct/qt6ct.conf", "qt5ct/qt5ct.conf"] {
        let p = format!("{base}/{variant}");
        if fs::metadata(&p).is_ok() {
            return Some(p);
        }
    }
    None
}

#[cfg(not(target_os = "macos"))]
fn read_kdeglobals(section: &str, key: &str) -> Option<String> {
    read_ini_key(
        &format!("{}/kdeglobals", xdg_config_home()?),
        Some(section),
        key,
    )
}

#[cfg(not(target_os = "macos"))]
fn qt_theme() -> Option<String> {
    if let Some(p) = qtct_conf_path() {
        if let Some(v) = read_ini_key(&p, Some("Appearance"), "style") {
            return Some(v);
        }
    }
    // kdeglobals reports the qtct shim as the style, which tells us nothing.
    read_kdeglobals("KDE", "widgetStyle").filter(|v| !v.contains("ct-style"))
}

#[cfg(not(target_os = "macos"))]
fn qt_icons() -> Option<String> {
    if let Some(p) = qtct_conf_path() {
        if let Some(v) = read_ini_key(&p, Some("Appearance"), "icon_theme") {
            return Some(v);
        }
    }
    read_kdeglobals("Icons", "Theme")
}

/// "Sans 11" -> "Sans (11pt)". Leaves anything that doesn't end in a plain
/// integer alone.
#[cfg(not(target_os = "macos"))]
fn font_pt_format(font: &str) -> String {
    let Some(pos) = font.rfind(' ') else {
        return font.to_string();
    };
    let size = &font[pos + 1..];
    if size.is_empty() || !size.bytes().all(|c| c.is_ascii_digit()) {
        return font.to_string();
    }
    format!("{} ({}pt)", &font[..pos], size)
}

#[cfg(not(target_os = "macos"))]
fn qt_font() -> Option<String> {
    let path = qtct_conf_path()?;
    let raw = read_ini_key(&path, Some("Fonts"), "general")?;
    // "Noto Sans,11,-1,5,50,0,0,0,0,0"
    let mut parts = raw.split(',');
    let family = parts.next()?;
    let size = parts.next()?;
    Some(format!("{family} ({size}pt)"))
}

/// append "value [tag]" to a comma-separated list, skipping empty values.
#[cfg(not(target_os = "macos"))]
fn append_tagged(dst: &mut String, val: Option<&str>, tag: &str) {
    let Some(val) = val.filter(|v| !v.is_empty()) else {
        return;
    };
    if !dst.is_empty() {
        dst.push_str(", ");
    }
    dst.push_str(&format!("{val} [{tag}]"));
}

/// GTK2 and GTK3 collapse to one "[GTK2/3]" entry when they agree.
#[cfg(not(target_os = "macos"))]
fn append_gtk_pair(dst: &mut String, gtk2: Option<&str>, gtk3: Option<&str>) {
    if let (Some(a), Some(b)) = (gtk2, gtk3) {
        if !a.is_empty() && a == b {
            append_tagged(dst, gtk3, "GTK2/3");
            return;
        }
    }
    append_tagged(dst, gtk2, "GTK2");
    append_tagged(dst, gtk3, "GTK3");
}

fn gather_theme(info: &mut Info) {
    #[cfg(target_os = "macos")]
    {
        let style = run("defaults read -g AppleInterfaceStyle 2>/dev/null")
            .map(|o| strip_eol(o.split('\n').next().unwrap_or("")).to_string())
            .filter(|s| !s.is_empty());
        info.add_info("Theme", style.as_deref().unwrap_or("Light"));
    }
    #[cfg(not(target_os = "macos"))]
    {
        let qt = qt_theme();
        let gtk2 = read_gtk2_setting("gtk-theme-name");
        let gtk3 = read_gtk3_setting("gtk-theme-name");
        let mut out = String::new();
        append_tagged(&mut out, qt.as_deref(), "Qt");
        append_gtk_pair(&mut out, gtk2.as_deref(), gtk3.as_deref());
        if !out.is_empty() {
            info.add_info("Theme", &out);
        }
    }
}

fn gather_icons(info: &mut Info) {
    #[cfg(target_os = "macos")]
    {
        info.add_info("Icons", "System");
    }
    #[cfg(not(target_os = "macos"))]
    {
        let qt = qt_icons();
        let gtk2 = read_gtk2_setting("gtk-icon-theme-name");
        let gtk3 = read_gtk3_setting("gtk-icon-theme-name");
        let mut out = String::new();
        append_tagged(&mut out, qt.as_deref(), "Qt");
        append_gtk_pair(&mut out, gtk2.as_deref(), gtk3.as_deref());
        if !out.is_empty() {
            info.add_info("Icons", &out);
        }
    }
}

fn gather_font(info: &mut Info) {
    #[cfg(target_os = "macos")]
    {
        let font = run("defaults read NSGlobalDomain AppleFont 2>/dev/null")
            .map(|o| strip_eol(o.split('\n').next().unwrap_or("")).to_string())
            .filter(|s| !s.is_empty());
        if let Some(font) = font {
            info.add_info("Font", &font);
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let qt = qt_font();
        let gtk2 = read_gtk2_setting("gtk-font-name").map(|f| font_pt_format(&f));
        let gtk3 = read_gtk3_setting("gtk-font-name").map(|f| font_pt_format(&f));
        let mut out = String::new();
        append_tagged(&mut out, qt.as_deref(), "Qt");
        append_gtk_pair(&mut out, gtk2.as_deref(), gtk3.as_deref());
        if !out.is_empty() {
            info.add_info("Font", &out);
        }
    }
}

fn gather_cursor(info: &mut Info) {
    #[cfg(not(target_os = "macos"))]
    {
        let Some(cursor) = read_gtk3_setting("gtk-cursor-theme-name") else {
            return;
        };
        match read_gtk3_setting("gtk-cursor-theme-size") {
            Some(size) => info.add_info("Cursor", &format!("{cursor} ({size}px)")),
            None => info.add_info("Cursor", &cursor),
        }
    }
    #[cfg(target_os = "macos")]
    {
        let _ = info;
    }
}

// --- macOS battery (IOKit) ---

#[cfg(target_os = "macos")]
mod iokit {
    use std::ffi::c_void;

    pub type CFTypeRef = *const c_void;
    pub type CFArrayRef = *const c_void;
    pub type CFDictionaryRef = *const c_void;
    pub type CFStringRef = *const c_void;
    pub type CFAllocatorRef = *const c_void;
    pub type CFIndex = isize;

    pub const K_CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;
    pub const K_CF_NUMBER_INT_TYPE: i32 = 9;

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        pub fn CFRelease(cf: CFTypeRef);
        pub fn CFArrayGetCount(array: CFArrayRef) -> CFIndex;
        pub fn CFArrayGetValueAtIndex(array: CFArrayRef, idx: CFIndex) -> *const c_void;
        pub fn CFDictionaryGetValue(dict: CFDictionaryRef, key: *const c_void) -> *const c_void;
        // The C return type is `Boolean` (= unsigned char). Rust's `bool` only
        // admits 0/1, and any other value would be UB.
        pub fn CFNumberGetValue(number: *const c_void, the_type: i32, value_ptr: *mut c_void)
            -> u8;
        pub fn CFBooleanGetValue(boolean: *const c_void) -> u8;
        pub fn CFStringCreateWithCString(
            alloc: CFAllocatorRef,
            c_str: *const libc::c_char,
            encoding: u32,
        ) -> CFStringRef;
    }

    #[link(name = "IOKit", kind = "framework")]
    extern "C" {
        pub fn IOPSCopyPowerSourcesInfo() -> CFTypeRef;
        pub fn IOPSCopyPowerSourcesList(blob: CFTypeRef) -> CFArrayRef;
        pub fn IOPSGetPowerSourceDescription(blob: CFTypeRef, ps: *const c_void)
            -> CFDictionaryRef;
    }
}

/// (capacity %, charging, minutes to empty). Capacity is -1 if unknown,
/// time to empty -1 if the system won't estimate it.
#[cfg(target_os = "macos")]
fn mac_battery() -> Option<(i64, bool, i64)> {
    use iokit::*;
    use std::ffi::c_void;

    unsafe {
        // CFSTR() is a compiler builtin in C, so build the keys by hand.
        let cfstr = |s: &str| -> CFStringRef {
            let c = CString::new(s).unwrap();
            CFStringCreateWithCString(std::ptr::null(), c.as_ptr(), K_CF_STRING_ENCODING_UTF8)
        };

        let info = IOPSCopyPowerSourcesInfo();
        if info.is_null() {
            return None;
        }
        let sources = IOPSCopyPowerSourcesList(info);
        if sources.is_null() || CFArrayGetCount(sources) == 0 {
            if !sources.is_null() {
                CFRelease(sources);
            }
            CFRelease(info);
            return None;
        }
        let src = IOPSGetPowerSourceDescription(info, CFArrayGetValueAtIndex(sources, 0));
        if src.is_null() {
            CFRelease(sources);
            CFRelease(info);
            return None;
        }

        let mut capacity: i32 = -1;
        let key = cfstr("Current Capacity");
        let cap_ref = CFDictionaryGetValue(src, key);
        if !cap_ref.is_null() {
            CFNumberGetValue(
                cap_ref,
                K_CF_NUMBER_INT_TYPE,
                &mut capacity as *mut i32 as *mut c_void,
            );
        }
        CFRelease(key);

        let key = cfstr("Is Charging");
        let chg_ref = CFDictionaryGetValue(src, key);
        let is_charging = !chg_ref.is_null() && CFBooleanGetValue(chg_ref) != 0;
        CFRelease(key);

        let mut time_to_empty: i32 = -1;
        let key = cfstr("Time to Empty");
        let tte_ref = CFDictionaryGetValue(src, key);
        if !tte_ref.is_null() {
            CFNumberGetValue(
                tte_ref,
                K_CF_NUMBER_INT_TYPE,
                &mut time_to_empty as *mut i32 as *mut c_void,
            );
        }
        CFRelease(key);

        CFRelease(sources);
        CFRelease(info);

        Some((capacity as i64, is_charging, time_to_empty as i64))
    }
}

// Geometry

#[derive(Clone, Copy)]
struct Point {
    px: f32,
    py: f32,
    pz: f32,
    nx: f32,
    ny: f32,
    nz: f32,
    color: i32,
}

/// Turn the logo's heightmap into the point cloud we spin. Each cell becomes
/// a stack of layers from the back face to the front, with normals that face
/// out of the relief on the caps and sideways on the extrusion.
fn build_points(logo: &Logo, cfg: &Config, sub_rows: usize) -> Vec<Point> {
    const SX: f32 = 0.07;
    const SY: f32 = 0.14;

    let rows = logo.rows;
    let cols = logo.cols;
    if rows == 0 || cols == 0 {
        return Vec::new();
    }

    let cx = (cols as f32 - 1.0) * 0.5;
    let cy = (rows as f32 - 1.0) * 0.5;
    let z_layers = ((6.0 * cfg.size_scale) as i32).max(6);

    // One row and column of zero padding so the gradient at the edges reads
    // empty space instead of running off the end.
    let stride = cols + 1;
    let mut hmap = vec![0f32; (rows + 1) * stride];
    for r in 0..rows {
        for c in 0..cols {
            hmap[r * stride + c] = logo.weight_at(r, c);
        }
    }
    let h = |r: usize, c: usize| hmap[r * stride + c];

    // Auto-scale depth when the user hasn't set it explicitly. Logos with low
    // height variance look flat — boost depth to compensate.
    let mut depth = cfg.depth;
    if !cfg.depth_user_set {
        let (mut sum, mut sum2, mut n) = (0f32, 0f32, 0usize);
        for r in 0..rows {
            for c in 0..cols {
                let v = h(r, c);
                if v > 0.0 {
                    sum += v;
                    sum2 += v * v;
                    n += 1;
                }
            }
        }
        if n > 0 {
            let mean = sum / n as f32;
            let variance = sum2 / n as f32 - mean * mean;
            let stddev = if variance > 0.0 { variance.sqrt() } else { 0.0 };
            // stddev ranges ~0.05 (flat/uniform) to ~0.3 (high contrast).
            // Scale depth inversely: flat logos get up to a 3x boost.
            if stddev < 0.25 {
                depth *= 1.0 + 2.0 * (0.25 - stddev) / 0.25;
            }
        }
    }
    let zmax = 0.18 * depth;

    // Surface normals from the heightmap gradient.
    let mut gn = vec![(0f32, 0f32, 1f32); rows * cols];
    for r in 0..rows {
        for c in 0..cols {
            if h(r, c) <= 0.0 {
                continue;
            }
            let dhdx = if c > 0 && c < cols - 1 {
                (h(r, c + 1) - h(r, c - 1)) * 0.5
            } else if c == 0 {
                h(r, c + 1) - h(r, c)
            } else {
                h(r, c) - h(r, c - 1)
            } / SX;

            let dhdy = if r > 0 && r < rows - 1 {
                (h(r + 1, c) - h(r - 1, c)) * 0.5
            } else if r == 0 {
                h(r + 1, c) - h(r, c)
            } else {
                h(r, c) - h(r - 1, c)
            } / SY;

            let (nnx, nny, nnz) = (-dhdx, dhdy, 1.0f32);
            let l = (nnx * nnx + nny * nny + nnz * nnz).sqrt();
            gn[r * cols + c] = (nnx / l, nny / l, nnz / l);
        }
    }

    // Subdivide the grid for larger sizes to avoid gaps. Sub-cell modes sample
    // a finer grid, so they need proportionally more points to fill it.
    let mut subdiv = ((cfg.size_scale * sub_rows as f32) as usize).max(sub_rows);

    // Set the point budget up front. Hitting MAX_POINTS mid-loop and leaving
    // through `break 'outer` used to cut the bottom off the logo; shrinking the
    // subdivision instead lowers quality evenly everywhere.
    let mut ink_cells = 0usize;
    for r in 0..rows {
        for c in 0..cols {
            if h(r, c) > 0.0 {
                ink_cells += 1;
            }
        }
    }
    let ink_cells = ink_cells.max(1) as u64;
    // u64 arithmetic so usize can't overflow on 32-bit targets (iSH i686, armv7).
    let budget = |sd: usize| -> u64 {
        ink_cells
            .saturating_mul(sd as u64)
            .saturating_mul(sd as u64)
            .saturating_mul(z_layers.max(1) as u64)
    };
    while subdiv > 1 && budget(subdiv) > MAX_POINTS as u64 {
        subdiv -= 1;
    }

    let mut points: Vec<Point> = Vec::with_capacity(budget(subdiv).min(MAX_POINTS as u64) as usize);
    'outer: for row in 0..rows {
        for col in 0..cols {
            let hc = h(row, col);
            if hc <= 0.0 {
                continue;
            }

            // Only interior cells get side layers. Edge cells (adjacent to
            // empty space) get front + back only, to avoid "tail" artifacts
            // during rotation.
            let mut is_edge = false;
            let mut edge_x = 0f32;
            let mut edge_y = 0f32;
            for dr in -1i32..=1 {
                for dc in -1i32..=1 {
                    if dr == 0 && dc == 0 {
                        continue;
                    }
                    let (nr, nc) = (row as i32 + dr, col as i32 + dc);
                    let nh = if nr >= 0 && nr < rows as i32 && nc >= 0 && nc < cols as i32 {
                        h(nr as usize, nc as usize)
                    } else {
                        0.0
                    };
                    if nh <= 0.0 {
                        is_edge = true;
                    }
                    if nh < hc {
                        edge_x += dc as f32;
                        edge_y += -dr as f32;
                    }
                }
            }
            let el = (edge_x * edge_x + edge_y * edge_y).sqrt();
            let (ex, ey) = if el > 1e-6 {
                (edge_x / el, edge_y / el)
            } else {
                (edge_x, edge_y)
            };

            let (gnx, gny, gnz) = gn[row * cols + col];

            for sr in 0..subdiv {
                for sc in 0..subdiv {
                    let frow = row as f32 + sr as f32 / subdiv as f32;
                    let fcol = col as f32 + sc as f32 / subdiv as f32;

                    // Interpolate height from neighbors
                    let mut ih = hc;
                    if sr > 0 || sc > 0 {
                        let fr = sr as f32 / subdiv as f32;
                        let fc = sc as f32 / subdiv as f32;
                        let nr = (row + usize::from(sr > 0)).min(rows - 1);
                        let nc = (col + usize::from(sc > 0)).min(cols - 1);
                        let h00 = h(row, col);
                        let h10 = h(nr, col);
                        let h01 = h(row, nc);
                        let h11 = h(nr, nc);
                        ih = h00 * (1.0 - fr) * (1.0 - fc)
                            + h10 * fr * (1.0 - fc)
                            + h01 * (1.0 - fr) * fc
                            + h11 * fr * fc;
                        if ih <= 0.0 {
                            continue;
                        }
                    }

                    let ox = (fcol - cx) * SX;
                    let oy = (cy - frow) * SY;
                    let zr = ih * zmax;

                    let layers = if is_edge || ih < 0.15 { 2 } else { z_layers };

                    for k in 0..layers {
                        if points.len() >= MAX_POINTS {
                            break 'outer;
                        }
                        let t = (k as f32 / (layers - 1) as f32) - 0.5;

                        // Uncolored logos are two-toned by which surface the
                        // point sits on: the front and back faces take the
                        // inner color, the extruded sides the outer one.
                        let color = if logo.has_ansi {
                            logo.color_at(row, col)
                        } else {
                            i32::from(k == 0 || k == layers - 1)
                        };

                        let (nx, ny, nz) = if k == 0 {
                            (gnx, gny, -gnz)
                        } else if k == layers - 1 {
                            (gnx, gny, gnz)
                        } else {
                            let tn = (k as f32 / (layers - 1) as f32) * 2.0 - 1.0;
                            let side = (1.0 - tn * tn).sqrt();
                            (ex * side, ey * side, tn)
                        };

                        points.push(Point {
                            px: ox,
                            py: oy,
                            pz: t * 2.0 * zr,
                            nx,
                            ny,
                            nz,
                            color,
                        });
                    }
                }
            }
        }
    }
    points
}

// Layout and rendering

/// Render buffers, one entry per sub-cell: z-buffer (0 = empty), luminance,
/// color.
struct Buffers {
    z: Vec<f32>,
    lum: Vec<f32>,
    color: Vec<i32>,
}

impl Buffers {
    fn new() -> Buffers {
        Buffers {
            z: vec![0.0; SUB_H * SUB_W],
            lum: vec![0.0; SUB_H * SUB_W],
            color: vec![0; SUB_H * SUB_W],
        }
    }

    fn clear(&mut self, render_height: i32, sub_rows: usize) {
        let n = (render_height.max(0) as usize * sub_rows * SUB_W).min(SUB_H * SUB_W);
        self.z[..n].fill(0.0);
        self.lum[..n].fill(0.0);
        self.color[..n].fill(0);
    }
}

/// Collapse one cell's sub-samples into a glyph, comparing the two ways the
/// cell can be drawn against the ink it should carry: the sub-cell block is
/// always full ink over the part it covers, the ramp spreads a lighter shade
/// over the whole cell. Picking by ink keeps crisp edges where the surface is
/// lit and stops dim edges from ringing the logo in a bright outline.
/// Returns None for an empty cell.
fn cell_glyph<'a>(
    buf: &Buffers,
    sh: &'a Shading,
    row: usize,
    col: usize,
) -> Option<(&'a str, i32)> {
    let x0 = col * sh.sub_cols;
    let y0 = row * sh.sub_rows;
    let total = sh.sub_rows * sh.sub_cols;
    let smax = sh.smax();

    let mut mask = 0usize;
    let mut bit = 0;
    let mut n = 0usize;
    let mut lsum = 0f32;
    let mut best = 0f32;
    let mut color = 0i32;

    for sr in 0..sh.sub_rows {
        for sc in 0..sh.sub_cols {
            let idx = (y0 + sr) * SUB_W + x0 + sc;
            let z = buf.z[idx];
            if z > 0.0 {
                mask |= 1 << bit;
                lsum += buf.lum[idx];
                n += 1;
                if z > best {
                    best = z;
                    color = buf.color[idx];
                }
            }
            bit += 1;
        }
    }
    if n == 0 {
        return None;
    }

    let coverage = n as f32 / total as f32;
    let ink = lsum / n as f32 * coverage;
    // Round to the nearest step: truncating biases every cell one level
    // lighter and leaves the top of the ramp unreachable.
    let ci = ((ink * smax as f32 + 0.5) as i32).clamp(0, smax as i32) as usize;

    if mask != (1 << total) - 1
        && (coverage - ink).abs() <= ((ci as f32 + 1.0) / sh.chars.len() as f32 - ink).abs()
    {
        let g = if sh.sub_rows == 3 {
            sh.sextants[mask].as_str()
        } else {
            QUADRANT_GLYPHS[mask]
        };
        return Some((g, color));
    }
    Some((sh.chars[ci].as_str(), color))
}

struct Layout {
    /// terminal width, 0 when unknown
    cols: i32,
    render_height: i32,
    /// Rows the logo may span (<= render_height).
    logo_height: i32,
    /// Canvas columns, shrinks on narrow terminals.
    anim_width: i32,
    /// Info below the logo instead of beside it.
    stacked: bool,
    /// Clip info lines to this many visible columns (-1 = no limit).
    info_clip_cols: i32,
    /// Info lines shown in the stacked layout.
    stacked_info_rows: i32,
    /// canvas row the info column starts on, side by side
    info_top: i32,
}

/// How many rows the spinning logo can reach, per row of logo_height. The
/// projection is y·k1/(z+K2) with k1 = 37/36 · logo_height; y stays within the
/// cloud's radius around the x axis and z within its full radius, whichever
/// way it turns.
fn logo_extent(points: &[Point], k2: f32) -> f32 {
    let (mut ry, mut r) = (0f32, 0f32);
    for p in points {
        ry = ry.max(p.py * p.py + p.pz * p.pz);
        r = r.max(p.px * p.px + p.py * p.py + p.pz * p.pz);
    }
    if points.is_empty() {
        return 0.8;
    }
    let (ry, r) = (ry.sqrt(), r.sqrt().min(k2 - 0.5));
    2.0 * (37.0 / 36.0) * ry / (k2 - r)
}

fn apply_layout(
    cfg: &Config,
    info: &Info,
    show_info: bool,
    footer: bool,
    logo_extent: f32,
) -> Layout {
    let (mut rows, cols) = get_term_size();
    // the last row stays free for the cursor, and the footer takes the one
    // above it
    if rows > 1 {
        rows -= 1;
    }
    if footer && rows > 1 {
        rows -= 1;
    }
    let line_count = info.lines.len() as i32;
    let mut ideal = 36;
    if cfg.height > 0 {
        ideal = cfg.height;
    } else if show_info && line_count > 0 {
        ideal = (line_count + 2).max(36);
    }
    ideal = (ideal as f32 * cfg.size_scale) as i32;
    ideal = ideal.clamp(20, MAX_HEIGHT);

    let mut l = Layout {
        cols,
        render_height: ideal,
        logo_height: ideal,
        anim_width: ANIM_WIDTH,
        stacked: false,
        info_clip_cols: -1,
        stacked_info_rows: 0,
        info_top: 1,
    };

    if cols <= 0 {
        if rows > 0 && l.render_height > rows {
            l.render_height = rows;
        }
        l.logo_height = l.render_height;
        return l;
    }

    let info_cols = if show_info { info.widest() as i32 } else { 0 };

    // Side by side is the layout; stacking is the last resort. When both
    // don't fit, the logo shrinks first, and then the info lines get clipped
    // before the logo is moved above them.
    const MIN_ANIM: i32 = 24;
    const MIN_INFO: i32 = 60;
    if show_info && cols < ANIM_WIDTH + GAP as i32 + info_cols {
        l.anim_width = cols - GAP as i32 - info_cols;
        if l.anim_width < MIN_ANIM && cols - GAP as i32 - MIN_ANIM >= MIN_INFO {
            l.anim_width = MIN_ANIM;
        } else if l.anim_width < 20 {
            l.stacked = true;
            l.anim_width = cols.min(ANIM_WIDTH);
        }
    } else if l.anim_width > cols {
        l.anim_width = cols;
    }

    let wcap = l.anim_width * 3 / 5;

    if l.stacked {
        if l.render_height > wcap {
            l.render_height = wcap;
        }
        l.stacked_info_rows = line_count;
        if rows > 0 {
            let budget = rows - line_count - 1;
            if budget < l.render_height {
                l.render_height = budget;
            }
            if l.render_height < 6 {
                l.render_height = 0;
            }
            let info_budget = rows
                - if l.render_height > 0 {
                    l.render_height + 1
                } else {
                    0
                };
            if l.stacked_info_rows > info_budget {
                l.stacked_info_rows = info_budget.max(0);
            }
        }
        l.logo_height = l.render_height;
        // one column short: the \x1b[K that ends the row would erase a
        // character sitting in the last one
        l.info_clip_cols = cols - 1;
    } else {
        if show_info {
            l.info_clip_cols = cols - l.anim_width - GAP as i32 - 1;
        }
        if rows > 0 && l.render_height > rows {
            l.render_height = rows;
        }
        l.logo_height = l.render_height.min(wcap);
        if !show_info {
            l.render_height = l.logo_height;
        } else {
            // The canvas only has to be as tall as the taller column: a fixed
            // 36-row floor left a band of blank rows under a short panel.
            if cfg.height <= 0 {
                let logo_rows = (l.logo_height as f32 * logo_extent).ceil() as i32 + 1;
                l.render_height = l.render_height.min((line_count + 2).max(logo_rows));
            }
            // center the info on the logo, or leave one row above it when
            // it's the taller of the two
            l.info_top = if line_count + 2 <= l.render_height {
                (l.render_height - line_count) / 2
            } else {
                1
            };
        }
    }
    l
}

/// `n` blank cells. A long run goes out as erase-characters plus a cursor
/// move, a dozen bytes however wide it is, instead of one space per cell —
/// the logo's canvas is mostly empty.
fn push_blanks(out: &mut Vec<u8>, n: &mut usize) {
    use std::io::Write;
    if *n >= 8 {
        let _ = write!(out, "\x1b[{n}X\x1b[{n}C", n = *n);
    } else {
        out.extend(std::iter::repeat(b' ').take(*n));
    }
    *n = 0;
}

/// One screen row in two parts: the logo, always exactly `anim_width`
/// cells, and whatever follows it (the info line, or a whole row of its own).
#[derive(Default)]
struct Row {
    logo: Vec<u8>,
    rest: Vec<u8>,
    /// 1-based column `rest` starts at
    rest_col: usize,
}

/// What was drawn last frame. Only the parts that changed are sent: the info
/// column mostly sits still while the logo spins next to it, so most frames
/// carry the logo and nothing else.
struct Screen {
    prev: Vec<Row>,
    prev_n: usize,
    cur: Vec<Row>,
    n: usize,
}

impl Screen {
    fn new() -> Screen {
        Screen {
            prev: Vec::new(),
            prev_n: 0,
            cur: Vec::new(),
            n: 0,
        }
    }

    fn begin(&mut self) {
        self.n = 0;
    }

    /// the next row, emptied, to draw into
    fn next_row(&mut self) -> &mut Row {
        if self.n == self.cur.len() {
            self.cur.push(Row::default());
        }
        let r = &mut self.cur[self.n];
        r.logo.clear();
        r.rest.clear();
        r.rest_col = 1;
        self.n += 1;
        r
    }

    /// forget what's on screen, after it was cleared
    fn invalidate(&mut self) {
        self.prev_n = 0;
    }

    /// The escapes that bring the terminal from last frame to this one, in
    /// one synchronized update (DEC mode 2026) so no frame shows half-drawn.
    /// Leaves the cursor under the last row, which is where the shell prompt
    /// should land however nyi exits.
    fn flush(&mut self, out: &mut Vec<u8>) {
        use std::io::Write;
        let mut changed = false;
        let mut begin = |out: &mut Vec<u8>| {
            if !changed {
                out.extend_from_slice(b"\x1b[?2026h");
                changed = true;
            }
        };
        for i in 0..self.n.max(self.prev_n) {
            let old = (i < self.prev_n).then(|| &self.prev[i]);
            let Some(new) = (i < self.n).then(|| &self.cur[i]) else {
                // the frame got shorter
                begin(out);
                let _ = write!(out, "\x1b[{};1H\x1b[K", i + 1);
                continue;
            };
            // The logo part is fixed-width, so it can be redrawn on its own
            // without touching what comes after it.
            let logo_same = old.is_some_and(|o| o.logo == new.logo);
            let rest_same = old.is_some_and(|o| o.rest == new.rest && o.rest_col == new.rest_col);
            let wrote_logo = !logo_same && !new.logo.is_empty();
            if wrote_logo {
                begin(out);
                let _ = write!(out, "\x1b[{};1H", i + 1);
                out.extend_from_slice(&new.logo);
            }
            if !rest_same || (!logo_same && new.logo.is_empty()) {
                begin(out);
                // the logo leaves the cursor right where the rest begins
                if !wrote_logo {
                    let _ = write!(out, "\x1b[{};{}H", i + 1, new.rest_col);
                }
                out.extend_from_slice(&new.rest);
                out.extend_from_slice(b"\x1b[K");
            }
        }
        if changed {
            let _ = write!(out, "\x1b[{};1H\x1b[?2026l", self.n + 1);
        }
        std::mem::swap(&mut self.prev, &mut self.cur);
        self.prev_n = self.n;
    }
}

// CLI

const HELP: &str = "\
Usage: nyi [options]

Options:
  -l, --logo <name>         Use a logo from fastfetch by name
                            Any logo fastfetch supports works, e.g.:
                              nixos, arch, gentoo, debian, ubuntu,
                              fedora, void, alpine, opensuse, manjaro,
                              proxmox, pop, linuxmint, endeavouros...
                            Run 'fastfetch --list-logos' to see all
                            nyi's own logo is 'triangle', which is also
                            what it draws when nothing else is found
  --rotate-x                Lock rotation to X axis only
  --rotate-y                Lock rotation to Y axis only
  -s, --speed <float>       Speed multiplier (default 1.0)
  --size <float>            Scale the logo (e.g. 2.0 for double size)
  --depth <float>           Scale the 3D depth (default 1.0)
  --height <n>              Override render height in rows
  --no-info                 Just the logo, no system info
  --no-color                Disable logo coloring
  --frames <n>              Stop after n frames (default 2000)
  --infinite                Run forever (keypress or Ctrl-C to exit)
  --shading-mode <mode>     ascii (.,-~:;=!*#$@, the default), or opt into
                            sub-cell blocks: sextants (2x3) or blocks (2x2)
  --shading-chars <str>     Custom shading ramp, supports UTF-8
  --box                     Draw a border box around the info block
  --style <style>           panel (the default): a header, a summary line
                            and usage bars; or classic: user@host and
                            Label: value lines
  --no-footer               Leave out the status bar under the panel
  --config <path>           Use this config file instead of the default
  --dump-logo <name>        Print a logo in logo.txt form, taken from
                            fastfetch. Run it where fastfetch is installed:
                              nyi --dump-logo apple > logo.txt
  --fetch-logo <name>       Same, but downloaded, for machines that can't
                            run fastfetch. Needs curl or wget, and is the
                            only thing in nyi that touches the network:
                              nyi --fetch-logo apple > ~/.config/nyi/logo.txt
  -V, --version             Show version
  -h, --help                Show this help

Config: $XDG_CONFIG_HOME/nyi/config (default ~/.config/nyi/config)
  List field names to show (in order), one per line.
  Comment out or remove fields to hide them.
  Available fields:
    os, host, kernel, uptime, packages, shell, display, wm,
    theme, icons, font, cursor, terminal, cpu, gpu, memory, swap,
    disk, ip, battery, locale, audio, colors
  With style=panel, os, kernel, packages and wm make up the summary
  line under the header instead of rows of their own.

  Extra disks:
    disk=/home               Show additional mount point
    disk=/data               (repeat for multiple mounts)

  Settings:
    label_color=<color>      Label color (red, green, yellow, blue,
                             magenta, cyan, white, or ANSI number)
    separator=<char>         Title separator character
    shading_mode=<mode>      ascii (default), blocks or sextants
    shading=<str>            Shading ramp characters
    logo_outer=<color>       Extruded side color
    logo_inner=<color>       Front/back face color
    light=<dir>              Light direction (top-left, top-right, top,
                             left, right, front, bottom-left, bottom-right)
    spin=<axes>              Rotation axes (x, y, or xy)
    speed=<float>            Rotation speed
    size=<float>             Logo scale
    depth=<float>            3D extrusion depth
    height=<n>               Render height in rows
    box=<0/1>                Draw a border box around the info block
    style=<style>            panel (default) or classic
    footer=<0/1>             Live status bar under the panel (default
                             on for panel, off for classic)

Logo: $XDG_CONFIG_HOME/nyi/logo.txt
  Custom ASCII/Unicode logo. Add '# distro: <name>' as the
  first line to set the color scheme.
";

struct Args {
    logo: Option<String>,
    rotate: Option<(bool, bool)>,
    speed: Option<f32>,
    size: Option<f32>,
    depth: Option<f32>,
    height: Option<i32>,
    show_info: bool,
    use_color: bool,
    max_frames: i32,
    shading_chars: Option<String>,
    shading_mode: Option<String>,
    box_flag: bool,
    config_path: Option<String>,
    style: Option<Style>,
    no_footer: bool,
}

impl Default for Args {
    fn default() -> Args {
        Args {
            logo: None,
            rotate: None,
            speed: None,
            size: None,
            depth: None,
            height: None,
            show_info: true,
            use_color: true,
            max_frames: 2000,
            shading_chars: None,
            shading_mode: None,
            box_flag: false,
            config_path: None,
            style: None,
            no_footer: false,
        }
    }
}

/// args
fn parse_args() -> Result<Option<Args>, String> {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut a = Args::default();
    let mut i = 0;

    while i < argv.len() {
        let arg = argv[i].as_str();
        let next = |i: &mut usize| -> Result<String, String> {
            if *i + 1 >= argv.len() {
                return Err(format!("nyi: option '{}' requires an argument", argv[*i]));
            }
            *i += 1;
            Ok(argv[*i].clone())
        };

        match arg {
            "--help" | "-h" => {
                print!("{HELP}");
                return Ok(None);
            }
            "--version" | "-V" => {
                println!(
                    "nyi {} \"{}\" ({}, {})",
                    version(),
                    CODENAME,
                    BUILD_ARCH,
                    BUILD_OS
                );
                return Ok(None);
            }
            "--dump-logo" => {
                dump_logo(&next(&mut i)?)?;
                return Ok(None);
            }
            "--fetch-logo" => {
                fetch_logo(&next(&mut i)?)?;
                return Ok(None);
            }
            "--logo" | "-l" => a.logo = Some(next(&mut i)?),
            "--rotate-x" => a.rotate = Some((true, false)),
            "--rotate-y" => a.rotate = Some((false, true)),
            "--speed" | "-s" => a.speed = Some(atof(&next(&mut i)?)),
            "--no-info" => a.show_info = false,
            "--no-color" => a.use_color = false,
            "--frames" => a.max_frames = atoi(&next(&mut i)?),
            "--infinite" => a.max_frames = 0,
            "--shading-chars" => a.shading_chars = Some(next(&mut i)?),
            "--shading-mode" => a.shading_mode = Some(next(&mut i)?),
            "--height" => a.height = Some(atoi(&next(&mut i)?)),
            "--size" => a.size = Some(atof(&next(&mut i)?)),
            "--depth" => a.depth = Some(atof(&next(&mut i)?)),
            "--box" => a.box_flag = true,
            "--style" => {
                let v = next(&mut i)?;
                a.style = Some(Style::from_name(&v).ok_or_else(|| {
                    format!("unknown style: {v} (classic or panel)\nTry 'nyi --help'")
                })?);
            }
            "--no-footer" => a.no_footer = true,
            "--config" => a.config_path = Some(next(&mut i)?),
            other => {
                return Err(format!("unknown option: {other}\nTry 'nyi --help'"));
            }
        }
        i += 1;
    }
    Ok(Some(a))
}

fn gather_field(f: Field, info: &mut Info, cfg: &Config) {
    if info.style == Style::Panel {
        panel::gather_field(f, info, cfg);
        return;
    }
    match f {
        Field::Os => gather_os(info),
        Field::Host => gather_host(info),
        Field::Kernel => gather_kernel(info),
        Field::Uptime => gather_uptime(info),
        Field::Packages => gather_packages(info),
        Field::Shell => gather_shell(info),
        Field::Display => gather_display(info),
        Field::Wm => gather_wm(info),
        Field::Theme => gather_theme(info),
        Field::Icons => gather_icons(info),
        Field::Font => gather_font(info),
        Field::Cursor => gather_cursor(info),
        Field::Terminal => gather_terminal(info),
        Field::Cpu => gather_cpu(info),
        Field::Gpu => gather_gpu(info),
        Field::Memory => gather_memory(info),
        Field::Swap => gather_swap(info),
        Field::Disk => gather_disk(info, cfg),
        Field::Ip => gather_ip(info),
        Field::Battery => gather_battery(info),
        Field::Locale => gather_locale(info),
        Field::Audio => gather_audio(info),
        Field::Colors => gather_colors(info),
    }
}

fn gather_colors(info: &mut Info) {
    info.add_line("");
    info.add_line(
        "\x1b[40m   \x1b[41m   \x1b[42m   \x1b[43m   \
         \x1b[44m   \x1b[45m   \x1b[46m   \x1b[47m   \x1b[0m",
    );
    info.add_line(
        "\x1b[100m   \x1b[101m   \x1b[102m   \x1b[103m   \
         \x1b[104m   \x1b[105m   \x1b[106m   \x1b[107m   \x1b[0m",
    );
}

/// one job's worth of lines: the header for None, a field otherwise.
fn gather_job(job: Option<Field>, cfg: &Config) -> Vec<String> {
    let mut info = Info::new(cfg);
    match job {
        None => match cfg.style {
            Style::Classic => gather_title(&mut info, cfg),
            Style::Panel => panel::gather_header(&mut info, cfg),
        },
        Some(f) => {
            info.current = Some(f);
            gather_field(f, &mut info, cfg);
        }
    }
    info.lines
}

/// Every field at once, one thread each. Nearly all of the startup time is
/// spent waiting on files and subprocesses (`--version`s, nix-store, lspci),
/// and none of them depend on each other.
fn gather_all(cfg: &Config) -> Info {
    let jobs: Vec<Option<Field>> = std::iter::once(None)
        .chain(cfg.order.iter().map(|f| Some(*f)))
        .collect();

    let results: Vec<Vec<String>> = std::thread::scope(|scope| {
        let handles: Vec<_> = jobs
            .iter()
            .map(|&job| {
                std::thread::Builder::new()
                    .spawn_scoped(scope, move || gather_job(job, cfg))
                    .map_err(|_| job)
            })
            .collect();
        handles
            .into_iter()
            .map(|h| match h {
                Ok(h) => h.join().unwrap_or_default(),
                // no threads to be had (a tight ulimit, an odd emulator): do
                // it here instead
                Err(job) => gather_job(job, cfg),
            })
            .collect()
    });

    let mut info = Info::new(cfg);
    for (job, lines) in jobs.into_iter().zip(results) {
        match job {
            None => info.header_lines = lines.len(),
            Some(f) if !lines.is_empty() => info.field_line[f.idx()] = info.lines.len() as i32,
            Some(_) => {}
        }
        for l in &lines {
            info.add_line(l);
        }
    }
    info
}

// Main

extern "C" fn cleanup_at_exit() {
    cleanup();
}

fn main() {
    restore_sigpipe();

    let args = match parse_args() {
        Ok(Some(a)) => a,
        Ok(None) => return,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };

    // Config
    let mut cfg = Config::default();
    let cfg_path = args
        .config_path
        .clone()
        .or_else(|| find_config_file("config"));
    if let Some(p) = &cfg_path {
        if !cfg.load(p) && args.config_path.is_some() {
            eprintln!("nyi: can't read config file: {p}");
            std::process::exit(1);
        }
    }
    if let Some(v) = args.height {
        cfg.height = v.min(MAX_HEIGHT);
    }
    if let Some(v) = args.size {
        cfg.size_scale = v.clamp(0.5, 5.0);
    }
    if let Some(v) = args.depth {
        cfg.depth = v.clamp(0.1, 10.0);
        cfg.depth_user_set = true;
    }
    if args.box_flag {
        cfg.box_on = true;
    }
    if let Some(st) = args.style {
        cfg.style = st;
    }
    if args.no_footer {
        cfg.footer = Some(false);
    }
    cfg.finalize();
    let cfg = std::sync::Arc::new(cfg);

    // Info runs alongside the logo lookup, which may be waiting on fastfetch.
    let info_thread = args.show_info.then(|| {
        let cfg = cfg.clone();
        std::thread::Builder::new().spawn(move || gather_all(&cfg))
    });

    let shading_mode = args
        .shading_mode
        .clone()
        .or_else(|| cfg.shading_mode.clone());
    let shading_chars = args.shading_chars.clone().or_else(|| cfg.shading.clone());
    let Some(shading) = select_shading(shading_mode.as_deref(), shading_chars.as_deref()) else {
        eprintln!(
            "unknown shading mode: {}\nTry 'nyi --help'",
            shading_mode.unwrap_or_default()
        );
        std::process::exit(1);
    };

    let speed = args
        .speed
        .unwrap_or(if cfg.speed > 0.0 { cfg.speed } else { 1.0 });
    let (rotate_x, rotate_y) = args.rotate.or(cfg.spin).unwrap_or((true, true));
    let show_info = args.show_info;
    let use_color = args.use_color;

    // Logo
    let mut distro;
    let raw_logo: Vec<String> = if let Some(name) = &args.logo {
        distro = name.clone();
        // Explicitly requested logo: fastfetch first, then the built-in table.
        // Only fastfetch's own names go to fastfetch — anything else would come
        // back as this machine's logo rather than the one that was asked for.
        let from_ff = ff_knows_logo(name)
            .then(|| load_logo_fastfetch(name))
            .flatten();
        match from_ff.or_else(|| builtin_logo(name)) {
            Some(rows) => rows,
            None => {
                eprintln!(
                    "nyi: no {name} logo found (install fastfetch for the full set); \
                     using the built-in {} logo.",
                    logos::DEFAULT_NAME
                );
                distro = logos::DEFAULT_NAME.to_string();
                default_logo()
            }
        }
    } else {
        // X.TXT
        let (file_rows, file_distro) = match load_logo_file() {
            Some((rows, d)) => (Some(rows), d),
            None => (None, None),
        };
        let mut id_like = None;
        match file_distro {
            Some(d) => distro = d,
            None => {
                let (id, like) = detect_distro();
                distro = id.unwrap_or_default();
                id_like = like;
            }
        }

        // Fastfetch logo. mobile::logo_override() speaks for the platforms whose
        // distro isn't what the machine is.
        let mut got = file_rows;
        // what we went looking for, which on iSH and friends is not the distro
        // the machine reports. The failure message has to name this, not the
        // distro, or it sends you hunting for the wrong logo.
        let mut wanted = distro.clone();
        if got.is_none() {
            if let Some(o) = mobile::logo_override() {
                wanted = o.to_string();
            }
            if !wanted.is_empty() {
                got = load_logo_fastfetch(&wanted);
                if got.is_some() {
                    distro = wanted.clone();
                }
            }
            if got.is_none() {
                if let Some(like) = &id_like {
                    for tok in like.split(' ').filter(|t| !t.is_empty()) {
                        if let Some(rows) = load_logo_fastfetch(tok) {
                            distro = tok.to_string();
                            got = Some(rows);
                            break;
                        }
                    }
                }
            }
        }
        // Without fastfetch there is nothing to fall back on but our own art.
        match got {
            Some(rows) => rows,
            None => {
                if !wanted.is_empty() {
                    eprintln!(
                        "nyi: no {wanted} logo found; using the built-in {} logo. \
                         Install fastfetch for the full set, or run \
                         'nyi --fetch-logo {wanted} > ~/.config/nyi/logo.txt'.",
                        logos::DEFAULT_NAME
                    );
                }
                distro = logos::DEFAULT_NAME.to_string();
                default_logo()
            }
        }
    };

    let logo = Logo::process(&raw_logo);
    let (outer, inner) = distro_colors(&distro);

    let color_outer = cfg
        .logo_outer
        .clone()
        .unwrap_or_else(|| outer.to_string())
        .into_bytes();

    let color_inner = cfg
        .logo_inner
        .clone()
        .unwrap_or_else(|| inner.to_string())
        .into_bytes();

    let mut info = match info_thread {
        Some(Ok(h)) => h.join().unwrap_or_else(|_| Info::new(&cfg)),
        Some(Err(_)) => gather_all(&cfg),
        None => Info::new(&cfg),
    };
    if cfg.box_on {
        info.box_wrap_lines();
    }
    cache::save();

    let footer = (show_info && cfg.footer_on()).then(panel::Footer::new);
    const K2: f32 = 5.5;
    let points = build_points(&logo, &cfg, shading.sub_rows);
    let logo_extent = logo_extent(&points, K2);
    let mut layout = apply_layout(&cfg, &info, show_info, footer.is_some(), logo_extent);

    // Render
    let mut a_angle = 0.0f32;
    let mut b_angle = 0.0f32;
    let mut k1 = 37.0 * layout.logo_height as f32 / 36.0;
    const FRAME_MS: u64 = 50;

    let (lx, ly, lz) = cfg.light;
    let (hx0, hy0, hz0) = (lx, ly, lz - 1.0);
    let hl0 = (hx0 * hx0 + hy0 * hy0 + hz0 * hz0).sqrt();
    let (hlx, hly, hlz) = (hx0 / hl0, hy0 / hl0, hz0 / hl0);

    let ansi_tbl: Vec<Vec<u8>> = (0..128)
        .map(|c| format!("\x1b[1;{c}m").into_bytes())
        .collect();

    install_signal_handlers();
    enter_raw_mode();
    unsafe { libc::atexit(cleanup_at_exit) };

    let mut buf: Vec<u8> = Vec::with_capacity(64 * 1024);
    let mut bufs = Buffers::new();
    let mut screen = Screen::new();

    out(b"\x1b[?25l\x1b[2J");

    let start = std::time::Instant::now();
    let mut next_frame = std::time::Duration::ZERO;
    let mut frame: i64 = 0;
    while args.max_frames == 0 || frame < args.max_frames as i64 {
        // The key check comes AFTER the frame: --frames 1 must draw a frame in
        // every case, and one leftover byte must not exit before drawing.
        if TERM_RESIZED.swap(false, Ordering::Relaxed) {
            // Always a full redraw: terminals reflow or scroll their contents
            // on resize, and the diff only knows what it last drew.
            layout = apply_layout(&cfg, &info, show_info, footer.is_some(), logo_extent);
            k1 = 37.0 * layout.logo_height as f32 / 36.0;
            out(b"\x1b[2J");
            screen.invalidate();
        }

        if show_info && frame > 0 && frame % 20 == 0 {
            info.refresh = true;
            for f in [
                Field::Uptime,
                Field::Memory,
                Field::Swap,
                Field::Battery,
                Field::Cpu,
            ] {
                if info.field_line[f.idx()] >= 0 {
                    info.current = Some(f);
                    gather_field(f, &mut info, &cfg);
                }
            }
            info.current = None;
            info.refresh = false;
        }

        bufs.clear(layout.render_height, shading.sub_rows);
        a_angle += if rotate_x { 0.04 * speed } else { 0.0 };
        b_angle += if rotate_y { 0.06 * speed } else { 0.0 };
        let (ca, sa) = (a_angle.cos(), a_angle.sin());
        let (cb, sb) = (b_angle.cos(), b_angle.sin());

        let y_center = layout.render_height as f32 * 0.5;
        let half_aw = layout.anim_width as f32 * 0.5;
        let k1x2 = k1 * 2.0;
        let aw = layout.anim_width.max(0) as usize;
        let sub_cols = shading.sub_cols;
        let sub_rows = shading.sub_rows;
        let max_xs = aw * sub_cols;
        let max_ys = layout.render_height.max(0) as usize * sub_rows;

        for p in &points {
            let y1 = p.py * ca - p.pz * sa;
            let z1 = p.py * sa + p.pz * ca;
            let x2 = p.px * cb + z1 * sb;
            let z2 = -p.px * sb + z1 * cb;
            let y2 = y1;

            let zc = z2 + K2;
            if zc < 0.1 {
                continue;
            }
            let ooz = 1.0 / zc;
            let xs = ((half_aw + k1x2 * x2 * ooz) * sub_cols as f32) as i32;
            let ys = ((y_center - k1 * y2 * ooz) * sub_rows as f32) as i32;
            if xs < 0 || xs >= max_xs as i32 || ys < 0 || ys >= max_ys as i32 {
                continue;
            }

            let idx = ys as usize * SUB_W + xs as usize;
            if ooz > bufs.z[idx] {
                // Only points that win the depth test get lit: most of the
                // cloud is hidden behind the front face, and rotating their
                // normals was wasted work.
                let ny1 = p.ny * ca - p.nz * sa;
                let nz1 = p.ny * sa + p.nz * ca;
                let nx2 = p.nx * cb + nz1 * sb;
                let nz2 = -p.nx * sb + nz1 * cb;
                let ny2 = ny1;

                let diff = (nx2 * lx + ny2 * ly + nz2 * lz).max(0.0);
                let spec_dot = (nx2 * hlx + ny2 * hly + nz2 * hlz).max(0.0);
                let mut spec = spec_dot * spec_dot;
                spec *= spec;
                spec *= spec;

                let l = (0.08 + 0.62 * diff + 0.30 * spec).min(1.0);

                bufs.z[idx] = ooz;
                bufs.lum[idx] = l;
                bufs.color[idx] = p.color;
            }
        }

        screen.begin();
        for i in 0..layout.render_height.max(0) {
            let row = i as usize;
            let r = screen.next_row();
            r.rest_col = aw + 1;
            let line = &mut r.logo;
            let mut blanks = 0usize;
            if !use_color {
                for j in 0..aw {
                    match cell_glyph(&bufs, &shading, row, j) {
                        None => blanks += 1,
                        Some((g, _)) => {
                            push_blanks(line, &mut blanks);
                            line.extend_from_slice(g.as_bytes());
                        }
                    }
                }
            } else {
                let mut prev_color = -1i32;
                for j in 0..aw {
                    match cell_glyph(&bufs, &shading, row, j) {
                        None => {
                            if prev_color != -1 {
                                line.extend_from_slice(b"\x1b[0m");
                                prev_color = -1;
                            }
                            blanks += 1;
                        }
                        Some((g, c)) => {
                            push_blanks(line, &mut blanks);
                            if c != prev_color {
                                if logo.has_ansi && c > 0 && c < 128 {
                                    line.extend_from_slice(&ansi_tbl[c as usize]);
                                } else if c == 1 {
                                    line.extend_from_slice(&color_inner);
                                } else {
                                    line.extend_from_slice(&color_outer);
                                }
                                prev_color = c;
                            }
                            line.extend_from_slice(g.as_bytes());
                        }
                    }
                }
                if prev_color != -1 {
                    line.extend_from_slice(b"\x1b[0m");
                }
            }
            push_blanks(line, &mut blanks);

            let fi = i - layout.info_top;
            if show_info && !layout.stacked && fi >= 0 && (fi as usize) < info.lines.len() {
                r.rest.extend_from_slice(&[b' '; GAP]);
                emit_clipped(&mut r.rest, &info.lines[fi as usize], layout.info_clip_cols);
            }
        }

        if layout.stacked && layout.stacked_info_rows > 0 {
            if layout.render_height > 0 {
                screen.next_row();
            }
            for l in info
                .lines
                .iter()
                .take(layout.stacked_info_rows.max(0) as usize)
            {
                emit_clipped(&mut screen.next_row().rest, l, layout.info_clip_cols);
            }
        }

        if let Some(f) = &footer {
            f.render(layout.cols, &mut screen.next_row().rest);
        }

        buf.clear();
        screen.flush(&mut buf);
        if !buf.is_empty() && !write_fd(libc::STDOUT_FILENO, &buf) {
            break;
        }
        frame += 1;

        // Pace against the clock rather than sleeping a fixed 50 ms after
        // the work, so the speed doesn't depend on how long a frame took. A
        // frame that runs long (a stalled terminal) resets the schedule
        // instead of being chased with a burst of catch-up frames.
        next_frame += std::time::Duration::from_millis(FRAME_MS);
        let now = start.elapsed();
        if next_frame > now {
            let wait = next_frame - now;
            sleep_ms(wait.as_millis() as i64);
        } else {
            next_frame = now;
        }
        if key_pressed() {
            break;
        }
    }

    cleanup();
}

// ---------------------------------------------------------------- tests
// Run with `cargo test`.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atoi_atof_c_semantics() {
        assert_eq!(atoi("42abc"), 42);
        assert_eq!(atoi("-7"), -7);
        assert_eq!(atoi("abc"), 0);
        assert_eq!(atoi(""), 0);
        assert!((atof("1.5xyz") - 1.5).abs() < 1e-6);
        assert_eq!(atof("abc"), 0.0);
    }

    #[test]
    fn ansi_is_invisible() {
        assert_eq!(visible_width("abc"), 3);
        assert_eq!(visible_width("\x1b[1;35mabc\x1b[0m"), 3);
        assert_eq!(visible_width("çğü"), 3);
        assert_eq!(visible_width(""), 0);
    }

    #[test]
    fn clip_never_cuts_mid_escape() {
        let mut out = Vec::new();
        emit_clipped(&mut out, "\x1b[1;35mabcdef\x1b[0m", 3);
        let s = String::from_utf8(out).unwrap();
        assert_eq!(visible_width(&s), 3);
    }

    #[test]
    fn heavier_glyphs_weigh_more() {
        assert!(char_weight("@") > char_weight("."));
        assert!(char_weight("█") > char_weight("░"));
        assert_eq!(char_weight(" "), 0.0);
    }

    #[test]
    fn shading_modes_have_right_geometry() {
        let a = select_shading(Some("ascii"), None).unwrap();
        assert_eq!((a.sub_rows, a.sub_cols), (1, 1));
        let b = select_shading(Some("blocks"), None).unwrap();
        assert_eq!((b.sub_rows, b.sub_cols), (2, 2));
        let s = select_shading(Some("sextants"), None).unwrap();
        assert_eq!((s.sub_rows, s.sub_cols), (3, 2));
        assert!(select_shading(Some("nope"), None).is_none());
        // an empty ramp falls back to "." instead of panicking
        assert!(!select_shading(Some("ascii"), Some(""))
            .unwrap()
            .chars
            .is_empty());
    }

    #[test]
    fn sextant_masks_map_to_real_glyphs() {
        let g = build_sextant_glyphs();
        assert_eq!(g[1], "🬀"); // top left only
        assert_eq!(g[21], "▌"); // left column
        assert_eq!(g[42], "▐"); // right column
        assert_eq!(g[62], "🬻"); // last sextant
                                // 0 and 63 are never asked for, but leaving them empty is a trap
        for (m, s) in g.iter().enumerate() {
            if m != 0 && m != 63 {
                assert!(!s.is_empty(), "mask {m} is empty");
            }
        }
    }

    #[test]
    fn quadrant_table_is_complete() {
        assert_eq!(QUADRANT_GLYPHS.len(), 16);
        assert_eq!(QUADRANT_GLYPHS[15], "█");
    }

    #[test]
    fn shell_interpolation_is_gated() {
        assert!(is_shell_safe("arch"));
        assert!(is_shell_safe("linux-mint"));
        assert!(!is_shell_safe("$(id)"));
        assert!(!is_shell_safe("a;b"));
        assert!(!is_shell_safe("a b"));
        assert!(!is_shell_safe(""));
    }

    #[test]
    fn logo_url_cannot_break_out_of_its_quotes() {
        assert!(is_url_safe(
            "https://raw.githubusercontent.com/elwrcl/nyi/main/logos"
        ));
        assert!(is_url_safe("file:///tmp/logos"));
        // the single quote is the only way out of '...' in sh
        assert!(!is_url_safe("https://x/';id;'"));
        assert!(!is_url_safe("https://x/ y"));
        assert!(!is_url_safe(""));
        assert!(!is_url_safe(&"h".repeat(512)));
    }

    #[test]
    fn config_survives_non_ascii_lines() {
        // byte 5 isn't a char boundary — this used to panic
        let dir = std::env::temp_dir().join("nyi-test-cfg");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("weird");
        std::fs::write(&p, "türkçe not\nabcdé\ndisk=/ö\nos\n").unwrap();
        let mut cfg = Config::default();
        assert!(cfg.load(p.to_str().unwrap()));
        assert!(cfg.enabled[Field::Os.idx()]);
    }

    #[test]
    fn color_names_and_raw_codes() {
        assert_eq!(color_code("magenta"), "35");
        assert_eq!(color_code("38;5;213"), "38;5;213");
        assert_eq!(light_preset("nope"), None);
        assert!(light_preset("top-left").is_some());
    }

    #[test]
    fn inline_hints_are_stripped() {
        assert_eq!(strip_inline_hint("white (red, green)"), "white");
        assert_eq!(strip_inline_hint("white"), "white");
    }
}
