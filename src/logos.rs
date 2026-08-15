// very bad pyramid

use crate::eq_ci;
const LOGO_TRIANGLE: &[&str] = &[
    "\x1b[92m                             M",
    "\x1b[92m                           NNMNN",
    "\x1b[92m                         mmNNMNNmm",
    "\x1b[93m                        dmmNNMNNmmd",
    "\x1b[93m                      hddmmNNMNNmmddh",
    "\x1b[93m                    yhhddmmNNMNNmmddhhy",
    "\x1b[91m                   yyhhddmmNNMNNmmddhhyy",
    "\x1b[91m                 ooyyhhddmmNNMNNmmddhhyyoo",
    "\x1b[91m                sooyyhhddmmNNMNNmmddhhyyoos",
    "\x1b[31m              =ssooyyhhddmmNNMNNmmddhhyyooss=",
    "\x1b[31m            ===ssooyyhhddmmNNMNNmmddhhyyooss===",
    "\x1b[31m           :===ssooyyhhddmmNNMNNmmddhhyyooss===:",
    "\x1b[95m         -::===ssooyyhhddmmNNMNNmmddhhyyooss===::-",
    "\x1b[95m        --::===ssooyyhhddmmNNMNNmmddhhyyooss===::--",
    "\x1b[95m      //--::===ssooyyhhddmmNNMNNmmddhhyyooss===::--//",
    "\x1b[95m    .///--::===ssooyyhhddmmNNMNNmmddhhyyooss===::--///.",
    "\x1b[94m   ..///--::===ssooyyhhddmmNNMNNmmddhhyyooss===::--///..",
    "\x1b[94m ``..///--::===ssooyyhhddmmNNMNNmmddhhyyooss===::--///..``",
    "\x1b[94m```..///--::===ssooyyhhddmmNNMNNmmddhhyyooss===::--///..```",
];

pub struct BuiltinLogo {
    pub names: &'static [&'static str],
    pub rows: &'static [&'static str],
}

pub const BUILTIN_LOGOS: &[BuiltinLogo] = &[BuiltinLogo {
    names: &["triangle", "pyramid", "nyi"],
    rows: LOGO_TRIANGLE,
}];

pub fn builtin_logo(name: &str) -> Option<Vec<String>> {
    BUILTIN_LOGOS
        .iter()
        .find(|l| l.names.iter().any(|n| eq_ci(n, name)))
        .map(|l| l.rows.iter().map(|s| s.to_string()).collect())
}

pub const DEFAULT_NAME: &str = "triangle";
pub fn default_logo() -> Vec<String> {
    LOGO_TRIANGLE.iter().map(|s| s.to_string()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{visible_width, Logo};

    #[test]
    fn default_logo_is_reachable_by_name() {
        assert_eq!(builtin_logo(DEFAULT_NAME), Some(default_logo()));
        assert!(builtin_logo("no-such-thing").is_none());
    }

    #[test]
    fn names_do_not_collide() {
        let mut seen: Vec<&str> = Vec::new();
        for l in BUILTIN_LOGOS {
            for n in l.names {
                assert!(!seen.contains(n), "{n} is listed twice");
                seen.push(n);
            }
        }
    }

    #[test]
    fn pyramid_fits_the_canvas() {
        let rows = default_logo();
        assert!(rows.len() >= 8);
        assert!(rows.iter().all(|r| visible_width(r) <= 60));
        let widths: Vec<usize> = rows.iter().map(|r| visible_width(r)).collect();
        assert_eq!(*widths.last().unwrap(), *widths.iter().max().unwrap());
    }

    #[test]
    fn pyramid_carries_its_own_colors() {
        let logo = Logo::process(&default_logo());
        assert!(logo.has_ansi, "the escapes did not survive parsing");
        let last = logo.rows - 1;
        assert!(logo.weight_at(last, logo.cols / 2) > logo.weight_at(last, 0));
    }
}
