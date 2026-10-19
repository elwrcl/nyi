// Small on-disk memo for answers that cost a subprocess but almost never
// change: `<bin> --version`, nix package counts, lspci names. Every key carries
// whatever would make it stale (a resolved store path, a binary's mtime and
// size), so there is no expiry to get wrong — a new key simply misses.
//
// A key no run has asked for in MAX_AGE saves is dropped, so entries for old
// store paths and replaced binaries fall out on their own, while a run that
// shows fewer fields (--no-info, a short config) doesn't throw the rest away.

use std::sync::{Arc, Mutex, OnceLock};

const MAX_AGE: u32 = 30;

struct Slot {
    key: String,
    val: Arc<OnceLock<String>>,
    /// saves since a run last asked for this key
    age: u32,
    used: bool,
}

struct Cache {
    slots: Vec<Slot>,
    dirty: bool,
}

fn cache_path() -> Option<String> {
    if std::env::var_os("NYI_NO_CACHE").is_some() {
        return None;
    }
    let base = match std::env::var("XDG_CACHE_HOME") {
        Ok(x) if !x.is_empty() => x,
        _ => format!("{}/.cache", crate::home_dir()?),
    };
    Some(format!("{base}/nyi/cache"))
}

fn cache() -> &'static Mutex<Cache> {
    static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
    CACHE.get_or_init(|| {
        let mut slots = Vec::new();
        if let Some(text) = cache_path().and_then(|p| crate::read_file(&p)) {
            // key \t value \t age
            for line in text.lines() {
                let Some((k, rest)) = line.split_once('\t') else {
                    continue;
                };
                let (v, age) = match rest.rsplit_once('\t') {
                    Some((v, a)) => (v, a.parse().unwrap_or(0)),
                    None => (rest, 0),
                };
                let val = Arc::new(OnceLock::new());
                let _ = val.set(v.to_string());
                slots.push(Slot {
                    key: k.to_string(),
                    val,
                    age,
                    used: false,
                });
            }
        }
        Mutex::new(Cache {
            slots,
            dirty: false,
        })
    })
}

/// The cached value for `key`, computing it with `f` on a miss. Two threads
/// asking for the same key at once run `f` once; the second one waits.
pub fn get_or(key: &str, f: impl FnOnce() -> String) -> String {
    let slot = {
        let mut c = cache().lock().unwrap_or_else(|e| e.into_inner());
        match c.slots.iter_mut().find(|s| s.key == key) {
            Some(s) => {
                s.used = true;
                s.val.clone()
            }
            None => {
                let val = Arc::new(OnceLock::new());
                c.slots.push(Slot {
                    key: key.to_string(),
                    val: val.clone(),
                    age: 0,
                    used: true,
                });
                c.dirty = true;
                val
            }
        }
    };
    slot.get_or_init(|| f().replace(['\t', '\n', '\r'], " "))
        .clone()
}

/// Write the cache back, aging the keys this run didn't ask for. A run that
/// asked for nothing leaves the file alone.
pub fn save() {
    let Some(path) = cache_path() else {
        return;
    };
    let c = cache().lock().unwrap_or_else(|e| e.into_inner());
    if !c.slots.iter().any(|s| s.used) {
        return;
    }
    // anything left unasked ages; anything asked again goes back to zero
    let aging = c.slots.iter().any(|s| !s.used || s.age > 0);
    if !c.dirty && !aging {
        return;
    }
    let mut out = String::new();
    for s in &c.slots {
        let age = if s.used { 0 } else { s.age + 1 };
        if age > MAX_AGE {
            continue;
        }
        if let Some(v) = s.val.get() {
            out.push_str(&format!("{}\t{v}\t{age}\n", s.key));
        }
    }
    let Some(dir) = path.rsplit_once('/').map(|(d, _)| d) else {
        return;
    };
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    // write-then-rename, so two nyi's finishing together can't interleave
    let tmp = format!("{path}.{}", std::process::id());
    if std::fs::write(&tmp, out).is_ok() && std::fs::rename(&tmp, &path).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}
