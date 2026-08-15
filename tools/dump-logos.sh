#!/bin/sh
# get logos from fastfetch

set -eu

NYI=${NYI:-./nyi}
OUT=${OUT:-logos}

command -v fastfetch >/dev/null 2>&1 || {
	echo "dump-logos: needs fastfetch on PATH" >&2
	exit 1
}
[ -x "$NYI" ] || {
	echo "dump-logos: $NYI is not executable (run make first, or set NYI=)" >&2
	exit 1
}

mkdir -p "$OUT"

names=$(fastfetch --print-logos 2>/dev/null | sed -n 's/^\([A-Za-z][A-Za-z0-9._+-]*\):$/\1/p' | sort -u)
[ -n "$names" ] || {
	echo "dump-logos: fastfetch listed no logos" >&2
	exit 1
}

written=0
skipped=0
for name in $names; do
	file=$(printf '%s' "$name" | tr 'A-Z' 'a-z')
	if "$NYI" --dump-logo "$name" >"$OUT/$file.txt" 2>/dev/null &&
		[ -s "$OUT/$file.txt" ]; then
		written=$((written + 1))
	else
		rm -f "$OUT/$file.txt"
		skipped=$((skipped + 1))
	fi
done

echo "dump-logos: wrote $written logos to $OUT/ ($skipped skipped)"
