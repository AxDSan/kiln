#!/bin/bash
# Compile every Kiln code sample in the documentation.
#
# A sample that does not build is worse than no sample: it is the first thing a
# newcomer copies, and it teaches them the language wrong. Fenced blocks with
# no language tag are treated as Kiln and must contain `module` to be
# considered a whole program — fragments are skipped deliberately.
set -uo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
KILN="${KILN:-$ROOT/target/release/kiln}"
[ -x "$KILN" ] || KILN="$ROOT/target/debug/kiln"
[ -x "$KILN" ] || { echo "build kiln first (cargo build)" >&2; exit 1; }

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

pass=0
fail=0

for doc in docs-site/src/*.md README.md docs/*.md; do
    [ -f "$doc" ] || continue
    # Split the file into fenced blocks, keeping only untagged ones that look
    # like a whole module.
    python3 - "$doc" "$WORK" <<'PY'
import re, sys, os
doc, work = sys.argv[1], sys.argv[2]
text = open(doc, encoding='utf-8').read()
stem = os.path.basename(doc).replace('.', '_')
for i, m in enumerate(re.finditer(r'^```([a-zA-Z]*)\n(.*?)^```', text, re.S | re.M)):
    lang, body = m.group(1), m.group(2)
    if lang not in ('', 'kiln'):
        continue
    if not re.search(r'^module\s+\w+', body, re.M):
        continue
    open(os.path.join(work, f'{stem}_{i}.kiln'), 'w', encoding='utf-8').write(body)
PY
done

# The landing page is hand-written HTML rather than Markdown, and its sample is
# the first Kiln most visitors read. Its <pre> block is highlighted with
# spans, so strip those and undo the entity escaping before compiling it. The
# first line says which language the block is in — `module` for Kiln 1.x,
# `namespace` for Kiln 2 — and it is compiled as that one.
python3 - docs-site/landing/index.html "$WORK" <<'PY'
import html, re, sys
page, work = sys.argv[1], sys.argv[2]
text = open(page, encoding='utf-8').read()
for i, m in enumerate(re.finditer(r'<pre>(.*?)</pre>', text, re.S)):
    body = html.unescape(re.sub(r'</?span[^>]*>', '', m.group(1)))
    ext = None
    if re.search(r'^module\s+\w+', body, re.M):
        ext = 'kiln'
    elif re.search(r'^namespace\s+\w+', body, re.M):
        ext = 'k2'
    if ext:
        open(f'{work}/landing_{i}.{ext}', 'w', encoding='utf-8').write(body + '\n')
PY

# Kiln 2 samples are tagged ```k2 and built with `kiln k2`. They are whole
# programs by construction — a fragment is not tagged.
for doc in docs-site/src/*.md README.md docs/*.md; do
    [ -f "$doc" ] || continue
    python3 - "$doc" "$WORK" <<'PY'
import re, sys, os
doc, work = sys.argv[1], sys.argv[2]
text = open(doc, encoding='utf-8').read()
stem = os.path.basename(doc).replace('.', '_')
for i, m in enumerate(re.finditer(r'^```k2\n(.*?)^```', text, re.S | re.M)):
    open(os.path.join(work, f'k2_{stem}_{i}.k2'), 'w', encoding='utf-8').write(m.group(1))
PY
done

shopt -s nullglob
for sample in "$WORK"/*.k2; do
    name="$(basename "$sample" .k2)"
    # `kiln k2` links the runtime itself when the program calls a command.
    kiln2="$WORK/$name.kiln"
    cp "$sample" "$kiln2"
    if out=$("$KILN" k2 "$kiln2" -o "$WORK/$name.bin" 2>&1); then
        printf '  %-44s PASS\n' "$name"
        pass=$((pass + 1))
    # A GUI sample needs the vendored UI stack to link. On a machine without it
    # — a fresh checkout, or CI — emit the IR instead: that still parses,
    # validates and lowers the whole sample, and it does not need the library's
    # implementation sources, only their metadata.
    elif grep -q "not vendored" <<<"$out" \
        && "$KILN" k2 "$kiln2" --emit-ir >/dev/null 2>&1; then
        printf '  %-44s PASS (validated, not linked)\n' "$name"
        pass=$((pass + 1))
    else
        printf '  %-44s FAIL\n' "$name"
        sed 's/^/      /' <<<"$out" | head -6
        fail=$((fail + 1))
    fi
    rm -f "$kiln2"
done

for sample in "$WORK"/*.kiln; do
    name="$(basename "$sample" .kiln)"
    # Build when we can — that checks the whole chain including the link. On a
    # machine without the vendored UI stack (a fresh checkout, or CI), fall back
    # to emitting IR, which still parses, validates and lowers the sample.
    if out=$("$KILN" build "$sample" -o "$WORK/$name.bin" 2>&1); then
        printf '  %-44s PASS\n' "$name"
        pass=$((pass + 1))
    elif grep -q "not vendored" <<<"$out" && out=$("$KILN" emit "$sample" 2>&1 >/dev/null); then
        printf '  %-44s PASS (validated, not linked)\n' "$name"
        pass=$((pass + 1))
    else
        printf '  %-44s FAIL\n' "$name"
        sed 's/^/      /' <<<"$out" | head -6
        fail=$((fail + 1))
    fi
done

echo
echo "  $pass sample(s) compiled, $fail failed"

# The landing page states how many kits, commands and components ship. Those
# numbers are the kind of claim that goes stale silently, so they are held to
# what the toolchain answers: bundled kits, and every command and component
# that core plus those kits declare. A count that included a project or user
# kit would depend on where the check happened to run.
counts_ok=1
kits=$("$KILN" kits | awk '/^kit: / && $4 == "bundled" { print $2 }')
"$KILN" commands > "$WORK/all.txt"
for k in $kits; do "$KILN" commands --use "$k" >> "$WORK/all.txt"; done
want_kits=$(wc -w <<<"$kits")
want_cmds=$(sed -n 's/^command: //p' "$WORK/all.txt" | sort -u | wc -l)
want_comps=$(sed -n 's/^component: //p' "$WORK/all.txt" | sort -u | wc -l)
for pair in "kit-count $want_kits" "command-count $want_cmds" "component-count $want_comps"; do
    set -- $pair
    have=$(sed -n "s/.*id=\"$1\">\([0-9]*\)<.*/\1/p" docs-site/landing/index.html)
    if [ "$have" != "$2" ]; then
        echo "  landing page says $1 is ${have:-missing}; the toolchain says $2"
        counts_ok=0
    fi
done
[ "$counts_ok" -eq 1 ] && echo "  landing counts match the toolchain ($want_kits kits, $want_cmds commands, $want_comps components)"

[ "$fail" -eq 0 ] && [ "$counts_ok" -eq 1 ]
