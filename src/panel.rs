// The panel style: a two-line header, a `·`-joined summary, then dim labels
// over values, with usage bars for memory, storage and power, and a live
// status bar along the bottom.
//
//   NixOS System, Linux 6.17.2
//   ThinkPad X1 Nano Gen 1, #1-NixOS (Oct 12)
//   ─────────────────────────────────────────
//   NixOS 26.11 · Linux 6.17.2 · zsh 5.9 · niri 25.08 · 1834 packages
//
//   host       nixos · elars
//   memory     ▰▰▰▰▱▱▱▱▱▱▱▱ 7.5 GiB of 15.7 GiB
//   ...
//
// The data comes from the same helpers the classic style uses; only the
// wording and the layout live here.

use super::*;

const DIM: &str = "\x1b[90m";
const BOLD: &str = "\x1b[1m";
const RESET: &str = "\x1b[0m";
const LABEL_W: usize = 11;
const BAR_W: usize = 12;

const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// A labeled row. Labels are lowercase whatever the caller passed, so the
/// classic gatherers (theme, font, ...) slot in unchanged.
pub fn row(label: &str, value: &str) -> String {
    let label = label.to_lowercase();
    format!("{DIM}{label:<LABEL_W$}{RESET} {value}")
}

fn dim(s: &str) -> String {
    format!("{DIM}{s}{RESET}")
}

/// the separator between facts on one line
fn dot() -> String {
    format!(" {DIM}·{RESET} ")
}

fn join(parts: &[String]) -> String {
    parts
        .iter()
        .filter(|p| !p.is_empty())
        .cloned()
        .collect::<Vec<_>>()
        .join(&dot())
}

/// The color bars and the header lean on. An explicit label_color wins, then
/// logo_outer, then the color the distro's logo is drawn in, so the panel
/// matches the shape spinning next to it.
fn accent(cfg: &Config) -> String {
    static ACCENT: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    ACCENT.get_or_init(|| pick_accent(cfg)).clone()
}

fn pick_accent(cfg: &Config) -> String {
    if cfg.label_color_set {
        return format!("\x1b[{}m", cfg.label_color);
    }
    if let Some(c) = &cfg.logo_outer {
        return c.clone();
    }
    #[cfg(target_os = "macos")]
    let id = "macos".to_string();
    #[cfg(not(target_os = "macos"))]
    let id = if mobile::is_android() {
        "android".to_string()
    } else {
        detect_distro_os_release().0.unwrap_or_default()
    };
    distro_colors(&id).0.to_string()
}

/// `▰▰▰▰▱▱▱▱` for a fraction in 0..=1.
fn bar(frac: f64, color: &str) -> String {
    let frac = if frac.is_finite() {
        frac.clamp(0.0, 1.0)
    } else {
        0.0
    };
    let mut filled = (frac * BAR_W as f64).round() as usize;
    // anything at all in use shows at least one cell
    if frac > 0.0 && filled == 0 {
        filled = 1;
    }
    format!(
        "{color}{}{RESET}{DIM}{}{RESET}",
        "▰".repeat(filled),
        "▱".repeat(BAR_W - filled)
    )
}

/// the bar color for a usage fraction: the accent, red once it's nearly full
fn usage_color(frac: f64, accent: &str) -> String {
    if frac >= 0.9 {
        "\x1b[31m".to_string()
    } else {
        accent.to_string()
    }
}

/// "7.5 GiB", "461 GiB", "1.8 TiB", "512 MiB"
fn size(bytes: f64) -> String {
    let gib = bytes / GIB;
    if gib >= 1024.0 {
        let tib = gib / 1024.0;
        if tib >= 100.0 {
            format!("{tib:.0} TiB")
        } else {
            format!("{tib:.1} TiB")
        }
    } else if gib >= 100.0 {
        format!("{gib:.0} GiB")
    } else if gib >= 1.0 {
        format!("{gib:.1} GiB")
    } else {
        format!("{:.0} MiB", bytes / 1048576.0)
    }
}

/// "5h 03m", "2d 4h 03m", "12m"
fn duration(secs: i64) -> String {
    let (d, h, m) = (secs / 86400, (secs % 86400) / 3600, (secs % 3600) / 60);
    if d > 0 {
        format!("{d}d {h}h {m:02}m")
    } else if h > 0 {
        format!("{h}h {m:02}m")
    } else {
        format!("{m}m")
    }
}

fn usage_row(info: &mut Info, label: &str, used: f64, total: f64, extra: &str, accent: &str) {
    let frac = used / total;
    let mut v = format!(
        "{} {} {}",
        bar(frac, &usage_color(frac, accent)),
        size(used),
        dim(&format!("of {}", size(total)))
    );
    if !extra.is_empty() {
        v.push_str(&dot());
        v.push_str(extra);
    }
    info.add_info(label, &v);
}

// header

#[cfg(not(target_os = "macos"))]
fn os_release_get(key: &str) -> Option<String> {
    let text = read_file("/etc/os-release")?;
    let prefix = format!("{key}=");
    text.lines()
        .find_map(|l| l.strip_prefix(prefix.as_str()))
        .and_then(os_release_val)
}

/// (name, name with version): ("NixOS", "NixOS 26.11")
fn os_names() -> (String, String) {
    #[cfg(target_os = "macos")]
    {
        let v = sysctl_str("kern.osproductversion").unwrap_or_default();
        (
            "macOS".to_string(),
            format!("macOS {v}").trim_end().to_string(),
        )
    }
    #[cfg(not(target_os = "macos"))]
    {
        if mobile::is_android() {
            return ("Android".to_string(), mobile::os_name());
        }
        let name = os_release_get("NAME").unwrap_or_else(|| "Linux".to_string());
        let full = match os_release_get("VERSION_ID") {
            Some(v) => format!("{name} {v}"),
            None => name.clone(),
        };
        if mobile::is_ish() {
            return (name, format!("{full} on iSH"));
        }
        (name, full)
    }
}

/// Firmware placeholders vendors leave in DMI.
#[cfg(not(target_os = "macos"))]
fn dmi_is_junk(s: &str) -> bool {
    const JUNK: [&str; 10] = [
        "to be filled",
        "system product name",
        "system version",
        "default string",
        "not applicable",
        "not specified",
        "type1productconfigid",
        "none",
        "o.e.m.",
        "invalid",
    ];
    s.is_empty() || JUNK.iter().any(|j| contains_ci(s, j))
}

/// Vendor names as people say them.
#[cfg(not(target_os = "macos"))]
fn short_vendor(v: &str) -> String {
    let l = v.to_ascii_lowercase();
    let short = if l.starts_with("lenovo") {
        "Lenovo"
    } else if l.starts_with("asustek") {
        "ASUS"
    } else if l.starts_with("hewlett-packard") || l == "hp" {
        "HP"
    } else if l.starts_with("micro-star") {
        "MSI"
    } else if l.starts_with("gigabyte") {
        "Gigabyte"
    } else if l.starts_with("dell") {
        "Dell"
    } else if l.starts_with("acer") {
        "Acer"
    } else if l.starts_with("apple") {
        "Apple"
    } else if l.starts_with("framework") {
        "Framework"
    } else if l.starts_with("samsung") {
        "Samsung"
    } else {
        return v
            .trim_end_matches(" Inc.")
            .trim_end_matches(" Corporation")
            .trim_end_matches(" Co., Ltd.")
            .to_string();
    };
    short.to_string()
}

/// (model, detail): ("Lenovo ThinkPad X1 Nano Gen 1", Some("20UN000EUS")).
fn machine() -> Option<(String, Option<String>)> {
    #[cfg(target_os = "macos")]
    {
        sysctl_str("hw.model").map(|m| (m, None))
    }
    #[cfg(not(target_os = "macos"))]
    {
        if mobile::is_android() {
            return mobile::host().map(|h| (h, None));
        }
        if let Some(m) = read_first_line("/proc/device-tree/model") {
            return Some((m.trim_end_matches('\0').to_string(), None));
        }
        let dmi = |k: &str| {
            read_first_line(&format!("/sys/class/dmi/id/{k}"))
                .map(|v| v.trim().to_string())
                .filter(|v| !dmi_is_junk(v))
        };
        let vendor = dmi("sys_vendor").map(|v| short_vendor(&v));
        let product = dmi("product_name");
        let version = dmi("product_version");
        // Lenovo puts the marketing name in product_version and the machine
        // type in product_name.
        let (model, detail) = match (&vendor, product, version) {
            (Some(v), Some(p), Some(ver)) if v == "Lenovo" => (ver, Some(p)),
            (_, Some(p), Some(ver)) if ver.chars().filter(|c| c.is_alphabetic()).count() >= 2 => {
                (p, Some(ver))
            }
            (_, Some(p), _) => (p, None),
            (_, None, Some(ver)) => (ver, None),
            (_, None, None) => return None,
        };
        let model = match vendor {
            Some(v) if !contains_ci(&model, &v) => format!("{v} {model}"),
            _ => model,
        };
        Some((model, detail))
    }
}

/// "#1-NixOS (Oct 12)" out of uname -v, which reads either
/// "#1-NixOS SMP PREEMPT_DYNAMIC Sun Oct 12 10:39:55 UTC 2025" or, for
/// reproducible builds, "#1-NixOS SMP PREEMPT_DYNAMIC @1791725270".
fn kernel_build() -> String {
    let v = uname().version;
    let toks: Vec<&str> = v.split_ascii_whitespace().collect();
    let Some(build) = toks.iter().find(|t| t.starts_with('#')) else {
        return String::new();
    };
    let mut date = String::new();
    for (i, t) in toks.iter().enumerate() {
        if let Some(epoch) = t.strip_prefix('@').and_then(|e| e.parse::<i64>().ok()) {
            if let Some(tm) = gm_time(epoch) {
                date = format!("{} {}", MONTHS[tm.tm_mon as usize % 12], tm.tm_mday);
            }
            break;
        }
        if MONTHS.contains(t) {
            if let Some(day) = toks
                .get(i + 1)
                .filter(|d| d.bytes().all(|c| c.is_ascii_digit()))
            {
                date = format!("{t} {day}");
                break;
            }
        }
    }
    if date.is_empty() {
        build.to_string()
    } else {
        format!("{build} ({date})")
    }
}

fn gm_time(epoch: i64) -> Option<libc::tm> {
    let t = epoch as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    (!unsafe { libc::gmtime_r(&t, &mut tm) }.is_null()).then_some(tm)
}

fn local_time() -> Option<libc::tm> {
    let t = unsafe { libc::time(std::ptr::null_mut()) };
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    (!unsafe { libc::localtime_r(&t, &mut tm) }.is_null()).then_some(tm)
}

pub fn gather_header(info: &mut Info, cfg: &Config) {
    let on = |f: Field| cfg.enabled[f.idx()];
    let u = uname();
    let (os_name, os_full) = os_names();

    let line1 = format!(
        "{BOLD}{os_name} System{RESET}{}",
        dim(&format!(", {} {}", u.sysname, u.release))
    );
    let model = machine().map(|(m, _)| m).unwrap_or_else(hostname);
    let build = kernel_build();
    let line2 = if build.is_empty() {
        model
    } else {
        format!("{model}{}", dim(&format!(", {build}")))
    };
    let width = visible_width(&line1).max(visible_width(&line2));
    info.add_line(&line1);
    info.add_line(&line2);
    if !cfg.separator.is_empty() {
        info.add_line(&dim(&"─".repeat(width)));
    }

    // The summary pulls from fields that would otherwise each take a row.
    // Each one is its own job so they overlap like the rows do.
    let os = || on(Field::Os).then(|| os_full.clone());
    let kernel = || on(Field::Kernel).then(|| format!("{} {}", u.sysname, u.release));
    let shell = || {
        on(Field::Shell)
            .then(shell_info)
            .flatten()
            .map(|(n, v)| format!("{n} {v}").trim_end().to_string())
    };
    let wm = || {
        on(Field::Wm)
            .then(wm_info)
            .flatten()
            .map(|(n, v, _)| format!("{n} {v}").trim_end().to_string())
    };
    let pkgs = || {
        on(Field::Packages)
            .then(|| package_entries().iter().map(|(n, _)| n).sum::<i64>())
            .filter(|n| *n > 0)
            .map(|n| format!("{n} packages"))
    };
    type Job<'a> = &'a (dyn Fn() -> Option<String> + Sync);
    let jobs: [Job; 5] = [&os, &kernel, &shell, &wm, &pkgs];
    let summary: Vec<String> = std::thread::scope(|s| {
        let handles: Vec<_> = jobs
            .iter()
            .map(|&job| {
                std::thread::Builder::new()
                    .spawn_scoped(s, job)
                    .map_err(|_| job)
            })
            .collect();
        handles
            .into_iter()
            .filter_map(|h| match h {
                Ok(h) => h.join().ok().flatten(),
                // out of threads: the same work, here
                Err(job) => job(),
            })
            .collect()
    });
    if !summary.is_empty() {
        info.add_line(&join(&summary));
    }
    info.add_line("");
}

// rows

/// "Intel(R) Core(TM) i5-3210M CPU" -> "Intel i5-3210M",
/// "11th Gen Intel(R) Core(TM) i7-1165G7" -> "Intel i7-1165G7",
/// "AMD Ryzen 7 5800X 8-Core Processor" -> "AMD Ryzen 7 5800X". The core
/// count and clock have their own places on the row.
fn cpu_name(name: &str) -> String {
    let intel = contains_ci(name, "intel");
    let mut words: Vec<&str> = Vec::new();
    for w in name.split_whitespace() {
        let w = w
            .trim_end_matches("(R)")
            .trim_end_matches("(TM)")
            .trim_end_matches("(tm)");
        let lw = w.to_ascii_lowercase();
        let ordinal = lw.len() > 2
            && lw.ends_with("th")
            && lw[..lw.len() - 2].bytes().all(|c| c.is_ascii_digit());
        if w.is_empty()
            || lw == "cpu"
            || lw == "processor"
            || lw.ends_with("-core")
            || (intel && (lw == "core" || lw == "gen" || lw == "intel" || ordinal))
        {
            continue;
        }
        words.push(w);
    }
    // "Intel" first, wherever the brand string put it
    if intel {
        words.insert(0, "Intel");
    }
    words.join(" ")
}

/// "Ivy Bridge mobile GT2 [HD Graphics 4000]" from Intel -> "Intel HD
/// Graphics", "GeForce RTX 4070
/// Max-Q / Mobile" -> "NVIDIA GeForce RTX 4070". Intel's integrated parts are
/// told apart by generation, not by the number after "Graphics".
fn gpu_name(g: &Gpu) -> String {
    // lspci puts the marketing name in brackets after the codename:
    // "Ivy Bridge mobile GT2 [HD Graphics 4000]"
    let name = match (g.name.rfind('['), g.name.rfind(']')) {
        (Some(l), Some(r)) if r > l + 1 && !g.name[l + 1..r].contains("AMD/ATI") => {
            &g.name[l + 1..r]
        }
        _ => g.name.as_str(),
    };
    let mut name = name.split(" / ").next().unwrap_or(name).to_string();
    for suffix in [" Max-Q", " Mobile", " Laptop GPU"] {
        if let Some(n) = name.strip_suffix(suffix) {
            name = n.to_string();
        }
    }
    if g.vendor == "Intel" {
        if let Some((head, tail)) = name.rsplit_once(' ') {
            if head.ends_with("Graphics") && tail.bytes().all(|c| c.is_ascii_digit()) {
                name = head.to_string();
            }
        }
    }
    if !g.vendor.is_empty() && !contains_ci(&name, g.vendor) {
        name = format!("{} {name}", g.vendor);
    }
    name
}

pub fn gather_field(f: Field, info: &mut Info, cfg: &Config) {
    let on = |f: Field| cfg.enabled[f.idx()];
    match f {
        // these make up the summary line
        Field::Os | Field::Kernel | Field::Packages | Field::Wm => {}

        Field::Host => {
            info.add_info("host", &join(&[hostname(), login_name()]));
            if let Some((model, detail)) = machine() {
                let detail = detail.map(|d| dim(&format!("({d})"))).unwrap_or_default();
                info.add_info("machine", format!("{model} {detail}").trim_end());
            }
        }

        Field::Cpu => {
            let Some(c) = cpu_info() else {
                return;
            };
            let plural = |n: i32, w: &str| format!("{n} {w}{}", if n == 1 { "" } else { "s" });
            let topo = if c.cores > 0 && c.cores != c.threads {
                format!(
                    "{}, {}",
                    plural(c.cores, "core"),
                    plural(c.threads, "thread")
                )
            } else if c.threads > 0 {
                plural(c.threads, "core")
            } else {
                String::new()
            };
            let ghz = if c.max_ghz > 0.0 {
                format!("{:.1} GHz", c.max_ghz)
            } else {
                String::new()
            };
            let temp = c.temp_c.map(|t| format!("{t:.0} °C")).unwrap_or_default();
            let profile = c.profile.as_deref().map(dim).unwrap_or_default();
            info.add_info(
                "processor",
                &join(&[cpu_name(&c.name), topo, ghz, temp, profile]),
            );
        }

        Field::Gpu => {
            let gpus = gpu_list();
            // the APIs belong to whichever GPU renders by default, which is
            // the first one more often than not
            let apis = if gpus.is_empty() {
                Vec::new()
            } else {
                gpu_apis()
            };
            for (i, g) in gpus.iter().enumerate() {
                let label = if i == 0 { "graphics" } else { "" };
                let mut parts = vec![gpu_name(g)];
                if i == 0 {
                    parts.extend(apis.iter().cloned());
                }
                info.add_info(label, &join(&parts));
            }
        }

        Field::Memory => {
            if let Some((used, total)) = mem_usage() {
                usage_row(info, "memory", used as f64, total as f64, "", &accent(cfg));
            }
        }

        Field::Swap => {
            if let Some((used, total)) = swap_usage() {
                usage_row(info, "swap", used as f64, total as f64, "", &accent(cfg));
            }
        }

        Field::Disk => {
            let accent = accent(cfg);
            let mounts = std::iter::once("/").chain(cfg.extra_disks.iter().map(String::as_str));
            let mut first = true;
            for path in mounts {
                let Some((used, total)) = disk_usage(path) else {
                    continue;
                };
                let fs = fstype_of(path);
                let extra = format!("{fs} {}", dim(&format!("({path})")));
                let label = if first { "storage" } else { "" };
                usage_row(info, label, used, total, extra.trim_start(), &accent);
                first = false;
            }
        }

        Field::Battery => {
            let Some(b) = battery() else {
                return;
            };
            let frac = b.capacity as f64 / 100.0;
            let color = if b.capacity < 20 {
                "\x1b[31m".to_string()
            } else {
                accent(cfg)
            };
            let status = b.status.to_lowercase();
            let state = match (status.as_str(), b.watts) {
                ("", _) => String::new(),
                ("not charging", _) => "plugged in".to_string(),
                (s, Some(w)) if w >= 0.1 => format!("{s} {w:.1} W"),
                (s, _) => s.to_string(),
            };
            let left = match (status.as_str(), b.mins_left) {
                ("discharging", Some(m)) if m > 0 => dim(&format!("{} left", duration(m * 60))),
                ("charging", Some(m)) if m > 0 => dim(&format!("{} to full", duration(m * 60))),
                _ => String::new(),
            };
            info.add_info(
                "power",
                &format!(
                    "{} {}",
                    bar(frac, &color),
                    join(&[format!("{}%", b.capacity), state, left])
                ),
            );
        }

        Field::Display => {
            let wm = on(Field::Wm)
                .then(wm_info)
                .flatten()
                .map(|(n, _, _)| n)
                .unwrap_or_default();
            for (i, d) in display_list().into_iter().enumerate() {
                let label = if i == 0 { "display" } else { "" };
                let name = match (d.name.is_empty(), d.inches) {
                    (true, _) => String::new(),
                    (false, 0) => dim(&d.name),
                    (false, n) => dim(&format!("{} {n}\"", d.name)),
                };
                let res = if d.scaled.is_empty() {
                    d.res.replace('x', "×")
                } else {
                    let scaled = dim(&format!("(as {})", d.scaled.replace('x', "×")));
                    format!("{} {scaled}", d.res.replace('x', "×"))
                };
                let first = if i == 0 { wm.clone() } else { String::new() };
                info.add_info(label, &join(&[first, res, format_hz(d.hz), name]));
            }
        }

        Field::Ip => {
            let parts: Vec<String> = ipv4_list()
                .into_iter()
                .filter(|(n, ..)| {
                    !(n.starts_with("docker")
                        || n.starts_with("br-")
                        || n.starts_with("veth")
                        || n.starts_with("virbr"))
                })
                .take(3)
                .map(|(name, addr, _, _)| format!("{name} {}", dim(&addr)))
                .collect();
            if !parts.is_empty() {
                info.add_info("network", &join(&parts));
            }
        }

        Field::Audio => {
            let devs = audio_devices();
            if devs.is_empty() {
                return;
            }
            let vol = match audio_volume() {
                Some((_, true)) => dim("muted"),
                Some((v, false)) => format!("{} {v}%", dim("volume")),
                None => String::new(),
            };
            let mut parts = devs;
            parts.push(vol);
            info.add_info("audio", &join(&parts));
        }

        Field::Shell => {
            let shell = shell_info()
                .map(|(n, v)| format!("{n} {v}").trim_end().to_string())
                .unwrap_or_default();
            let term = on(Field::Terminal)
                .then(terminal_info)
                .flatten()
                .map(|(n, _)| n)
                .unwrap_or_default();
            if !shell.is_empty() || !term.is_empty() {
                info.add_info("shell", &join(&[shell, term]));
            }
        }

        Field::Terminal => {
            // folded into the shell row when that one is shown
            if !on(Field::Shell) {
                if let Some((n, v)) = terminal_info() {
                    info.add_info("terminal", format!("{n} {v}").trim_end());
                }
            }
        }

        Field::Uptime => {
            if let Some(s) = uptime_secs() {
                info.add_info("uptime", &duration(s));
            }
        }

        Field::Theme => gather_theme(info),
        Field::Icons => gather_icons(info),
        Field::Font => gather_font(info),
        Field::Cursor => gather_cursor(info),
        Field::Locale => gather_locale(info),
        Field::Colors => gather_colors(info),
    }
}

// footer

#[cfg(target_os = "macos")]
fn load_avg() -> Option<[f64; 3]> {
    let mut l = [0f64; 3];
    (unsafe { libc::getloadavg(l.as_mut_ptr(), 3) } == 3).then_some(l)
}

/// sysinfo(2) rather than getloadavg(3), which Android's libc doesn't have
#[cfg(not(target_os = "macos"))]
fn load_avg() -> Option<[f64; 3]> {
    let mut si: libc::sysinfo = unsafe { std::mem::zeroed() };
    if unsafe { libc::sysinfo(&mut si) } != 0 {
        return None;
    }
    let f = |v: libc::c_ulong| v as f64 / 65536.0;
    Some([f(si.loads[0]), f(si.loads[1]), f(si.loads[2])])
}

/// The status bar under the panel: date, a ticking clock, who, and the load.
pub struct Footer {
    who: String,
}

impl Footer {
    pub fn new() -> Footer {
        Footer {
            who: format!("{}@{}", login_name(), hostname()),
        }
    }

    /// One row, padded to `cols`, ready to write.
    pub fn render(&self, cols: i32, out: &mut Vec<u8>) {
        const BAR: &str = "\x1b[48;5;236;38;5;252m";
        const SOFT: &str = "\x1b[38;5;245m";
        let clock = match local_time() {
            Some(tm) => format!(
                "{} {:2} {} {}  {:02}:{:02}:{:02}",
                DAYS[tm.tm_wday as usize % 7],
                tm.tm_mday,
                MONTHS[tm.tm_mon as usize % 12],
                tm.tm_year + 1900,
                tm.tm_hour,
                tm.tm_min,
                tm.tm_sec
            ),
            None => String::new(),
        };
        let load = match load_avg() {
            Some(l) => format!("load {:.2} {:.2} {:.2}", l[0], l[1], l[2]),
            None => String::new(),
        };
        let left = format!(" {clock}   {}", self.who);
        let right = format!("{load} ");
        let lw = visible_width(&left);
        let rw = visible_width(&right);
        // no width to pad to: just the two halves, a little apart
        // one short of the edge: the \x1b[K after the row would otherwise
        // erase the last cell the cursor sits on
        let cols = if cols > 1 {
            cols as usize - 1
        } else {
            lw + rw + 2
        };

        let line = if lw + rw + 2 <= cols {
            format!(
                "{BAR}{left}{}{SOFT}{right}{RESET}",
                " ".repeat(cols - lw - rw)
            )
        } else {
            format!("{BAR}{left}{}{RESET}", " ".repeat(cols.saturating_sub(lw)))
        };
        emit_clipped(out, &line, cols as i32);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_names_lose_the_noise() {
        assert_eq!(cpu_name("Intel(R) Core(TM) i5-3210M CPU"), "Intel i5-3210M");
        assert_eq!(
            cpu_name("11th Gen Intel(R) Core(TM) i7-1165G7"),
            "Intel i7-1165G7"
        );
        assert_eq!(
            cpu_name("AMD Ryzen 7 5800X 8-Core Processor"),
            "AMD Ryzen 7 5800X"
        );
    }

    #[test]
    fn gpu_names_are_short() {
        let g = |name: &str, vendor| Gpu {
            name: name.to_string(),
            kind: "",
            vendor,
        };
        let intel = g("Ivy Bridge mobile GT2 [HD Graphics 4000]", "Intel");
        assert_eq!(gpu_name(&intel), "Intel HD Graphics");
        let nv = g("AD106M [GeForce RTX 4070 Max-Q / Mobile]", "NVIDIA");
        assert_eq!(gpu_name(&nv), "NVIDIA GeForce RTX 4070");
        let amd = g("Navi 48 [Radeon RX 9070 XT]", "AMD");
        assert_eq!(gpu_name(&amd), "AMD Radeon RX 9070 XT");
        let arc = g("DG2 [Arc A770]", "Intel");
        assert_eq!(gpu_name(&arc), "Intel Arc A770");
    }
}
