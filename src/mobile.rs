// for mobile eg; termux, i$h, newterm.

#[cfg(not(target_os = "macos"))]
use crate::{contains_ci, count_file_lines, is_shell_safe, run, strip_eol, uname};
#[cfg(not(target_os = "macos"))]
use std::fs;

#[cfg(not(target_os = "macos"))]
pub fn is_android() -> bool {
    static CACHE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *CACHE.get_or_init(|| {
        cfg!(target_os = "android")
            || std::env::var_os("TERMUX_VERSION").is_some()
            || fs::metadata("/system/build.prop").is_ok()
            || std::env::var("PREFIX").is_ok_and(|p| p.contains("com.termux"))
    })
}

#[cfg(not(target_os = "macos"))]
pub fn is_ish() -> bool {
    static CACHE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *CACHE.get_or_init(|| fs::metadata("/proc/ish").is_ok() || uname().release.contains("ish"))
}

/// android `getprop` is on PATH under termux.
#[cfg(not(target_os = "macos"))]
fn getprop(key: &str) -> Option<String> {
    if !is_shell_safe(key) {
        return None;
    }
    let out = run(&format!("getprop {key} 2>/dev/null"))?;
    let v = strip_eol(out.split('\n').next()?).trim().to_string();
    if v.is_empty() {
        None
    } else {
        Some(v)
    }
}

/// android has no /etc/os-release; the version lives in the properties.
#[cfg(not(target_os = "macos"))]
pub fn os_name() -> String {
    match (
        getprop("ro.build.version.release"),
        getprop("ro.build.version.sdk"),
    ) {
        (Some(r), Some(s)) => format!("Android {r} (API {s})"),
        (Some(r), None) => format!("Android {r}"),
        _ => "Android".to_string(),
    }
}

#[cfg(not(target_os = "macos"))]
pub fn host() -> Option<String> {
    let brand = getprop("ro.product.manufacturer").or_else(|| getprop("ro.product.brand"));
    let model = getprop("ro.product.model")?;
    Some(match brand {
        Some(b) if !contains_ci(&model, &b) => format!("{b} {model}"),
        _ => model,
    })
}

/// termux keeps the dpkg database under $PREFIX, not /var/lib.
#[cfg(not(target_os = "macos"))]
pub fn package_count() -> Option<String> {
    let prefix = std::env::var("PREFIX").ok()?;
    let n = count_file_lines(&format!("{prefix}/var/lib/dpkg/status"), Some("Package:"));
    if n > 0 {
        Some(format!("{n} (dpkg)"))
    } else {
        None
    }
}

/// walking the process tree finds nothing here termux is an 'seperate' app.
pub fn terminal() -> Option<String> {
    let v = std::env::var("TERMUX_VERSION").ok()?;
    if v.is_empty() {
        None
    } else {
        Some(format!("Termux {v}"))
    }
}

pub fn logo_override() -> Option<&'static str> {
    #[cfg(not(target_os = "macos"))]
    {
        if is_ish() {
            return Some("apple");
        }
    }
    None
}
