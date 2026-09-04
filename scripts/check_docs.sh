#!/usr/bin/env bash
# Enforces the sibling-.MD documentation convention from CLAUDE.md, across
# the whole crates/ tree:
#   - every crates/**/*.rs file has a sibling <stem>.MD beside it
#   - every crates/**/*.MD file has a sibling <stem>.rs beside it
#   - every such .MD file's first line is exactly "# <stem>"
#   - every such .MD file has a "## Key Components" heading
#   - every such .MD file has a "## Usage Example" heading
#   - every `pub fn`/`struct`/`enum`/`trait`/`const`/`type` name declared in
#     a .rs file is mentioned somewhere in its sibling .MD, unless waived by
#     a `<!-- check_docs: allow-undocumented NAME... -->` line in that .MD
#   - every crates/*/Cargo.toml directory (i.e. every crate) has a
#     crates/<crate>/README.md
#   - no such .rs file has a `///` or `//!` doc comment, or a `//` comment
#     that isn't `// SAFETY:` or `// TODO(`
#   - every `M<number>`/`M<number>.<number>` under crates/ or docs/ matches
#     a heading in docs/ROADMAP.md
#   - no tracked file under crates/ cites `.claude/` (gitignored working
#     state a fresh clone does not have); docs/ is exempt, since the agent
#     flow diagrams cite it deliberately
#   - every `todo!(` site under crates/**/*.rs is named in CLAUDE.md's
#     "Known scaffolding" list, by its `crate::Type::method`
#
# The comment check tracks state line by line rather than judging each
# comment line in isolation: a `// SAFETY:`/`// TODO(` block is often
# several lines of prose, and only the first line carries the prefix - its
# continuation lines are ordinary `//` comments that are allowed only
# because the block they continue was allowed. A line with no comment (or
# a blank line) ends the block, so a plain `//` comment can never borrow
# permission from an unrelated SAFETY/TODO block earlier in the file.
#
# Matching is done with bash's own `[[ =~ ]]` rather than piping each line
# through `grep`/`sed`: under `set -e`, a per-line external pipeline that
# legitimately fails to match (i.e. most ordinary code lines, which have
# no `//` at all) aborts the whole script at the first such line, since a
# failed command substitution is itself a failing command. A regex used as
# an `if`/`while` condition doesn't have that problem - a false match is
# just the branch not taken, not an error.
set -euo pipefail
cd "$(dirname "$0")/.."

status=0

fail() {
    echo "check_docs: $1" >&2
    status=1
}

while IFS= read -r rs; do
    md="${rs%.rs}.MD"
    [ -f "$md" ] || fail "$rs has no sibling $md"
done < <(find crates -name '*.rs')

while IFS= read -r md; do
    rs="${md%.MD}.rs"
    [ -f "$rs" ] || fail "$md has no sibling $rs"
    stem="$(basename "$rs" .rs)"
    first_line="$(head -n 1 "$md")"
    [ "$first_line" = "# $stem" ] || fail "$md's first line is '$first_line', expected '# $stem'"
    grep -q '^## Key Components$' "$md" || fail "$md is missing the '## Key Components' heading"
    grep -q '^## Usage Example$' "$md" || fail "$md is missing the '## Usage Example' heading"
done < <(find crates -name '*.MD')

# Public-item coverage: every `pub fn`/`struct`/`enum`/`trait`/`const`/`type`
# declared in a .rs file must be named somewhere in its sibling .MD, unless
# explicitly waived via a `<!-- check_docs: allow-undocumented NAME... -->`
# line in that .MD. Matching `pub (fn|struct|enum|trait|const|type) ` (with
# the space) rather than just `pub` excludes `pub(crate)`/`pub(super)`,
# which aren't public outside the crate/module.
while IFS= read -r rs; do
    md="${rs%.rs}.MD"
    [ -f "$md" ] || continue

    allowed=" "
    allow_line="$(grep -m1 '<!-- check_docs: allow-undocumented ' "$md" || true)"
    if [ -n "$allow_line" ]; then
        allow_line="${allow_line#*allow-undocumented}"
        allow_line="${allow_line%%-->*}"
        allowed=" $allow_line "
    fi

    while IFS= read -r name; do
        [ -n "$name" ] || continue
        case "$allowed" in
            *" $name "*) continue ;;
        esac
        grep -q -F "$name" "$md" || fail "$rs: public item '$name' is not mentioned in $md"
    done < <(grep -hoE '\bpub (fn|struct|enum|trait|const|type) [A-Za-z_][A-Za-z0-9_]*' "$rs" | awk '{print $NF}')
done < <(find crates -name '*.rs')

while IFS= read -r toml; do
    dir="$(dirname "$toml")"
    [ -f "$dir/README.md" ] || fail "$dir has a Cargo.toml but no README.md"
done < <(find crates -name 'Cargo.toml')

comment_re='(^|[[:space:]])(//.*)$'

while IFS= read -r rs; do
    in_block=0
    while IFS= read -r line; do
        if [[ "$line" =~ $comment_re ]]; then
            comment="${BASH_REMATCH[2]}"
        else
            in_block=0
            continue
        fi
        case "$comment" in
            '// SAFETY:'*|'// TODO('*)
                in_block=1
                ;;
            '///'*|'//!'*)
                fail "$rs: disallowed comment: $comment"
                in_block=0
                ;;
            '//'*)
                if [ "$in_block" -ne 1 ]; then
                    fail "$rs: disallowed comment: $comment"
                fi
                ;;
        esac
    done < "$rs"
done < <(find crates -name '*.rs')

# Milestone identifiers under crates/ or docs/ must resolve to a heading in
# docs/ROADMAP.md. The roadmap renumbers milestones as priorities change,
# so a stale reference is expected to appear from time to time; this check
# only catches one resolving to nothing, not one that shifted onto a
# still-live but wrong milestone.
mapfile -t roadmap_milestones < <(grep -oE '^#{2,3} M[0-9]+(\.[0-9]+)?' docs/ROADMAP.md | awk '{print $2}')

is_known_milestone() {
    local m="$1" known
    for known in "${roadmap_milestones[@]}"; do
        [ "$m" = "$known" ] && return 0
    done
    return 1
}

while IFS= read -r f; do
    while IFS= read -r m; do
        [ -n "$m" ] || continue
        is_known_milestone "$m" || fail "$f: milestone $m has no docs/ROADMAP.md heading"
    done < <(grep -ahoE '\bM[0-9]+(\.[0-9]+)?\b' "$f" | sort -u)
done < <(find crates docs -type f \( -name '*.rs' -o -name '*.MD' -o -name '*.md' -o -name '*.mmd' \))

# .claude/ is gitignored working state; a tracked file under crates/ citing
# it promises what a fresh clone cannot deliver. docs/ is exempt: the agent
# flow diagrams cite it deliberately.
while IFS= read -r hit; do
    fail "$hit: cites gitignored .claude/ working state"
done < <(grep -rn '\.claude/' crates/ || true)

# Every todo!( site under crates/**/*.rs must be named in CLAUDE.md's
# "Known scaffolding" list, matched on the crate::Type::method identifier
# each bullet carries rather than by parsing prose.
mapfile -t scaffolding_bullets < <(awk '
/^## Known scaffolding/ { flag = 1; next }
/^## / { if (flag) exit }
flag {
    if ($0 ~ /^- /) {
        if (cur != "") print cur
        cur = $0
    } else if (cur != "") {
        cur = cur " " $0
    }
}
END { if (cur != "") print cur }
' CLAUDE.md)

bullet_spans() {
    grep -oE '`[^`]+`' <<<"$1" | sed -e 's/^`//' -e 's/`$//'
}

bullet_covers_todo() {
    local bullet="$1" crate="$2" type="$3" method="$4" span have_type=0
    while IFS= read -r span; do
        [ -n "$span" ] || continue
        if [[ "$span" =~ ^${crate}::([A-Za-z0-9_]+::)*${type}::${method}$ ]]; then
            return 0
        fi
        if [[ "$span" =~ ^${crate}::([A-Za-z0-9_]+::)*${type}$ ]]; then
            have_type=1
        fi
    done < <(bullet_spans "$bullet")
    if [ "$have_type" -eq 1 ]; then
        while IFS= read -r span; do
            [ "$span" = "$method" ] && return 0
        done < <(bullet_spans "$bullet")
    fi
    return 1
}

todo_is_declared() {
    local crate="$1" type="$2" method="$3" b
    for b in "${scaffolding_bullets[@]}"; do
        bullet_covers_todo "$b" "$crate" "$type" "$method" && return 0
    done
    return 1
}

# Walks a .rs file tracking brace depth to find, for each todo!( site, the
# nearest enclosing impl's type and the nearest enclosing fn's name. Generic
# parameter lists are stripped from each candidate line first so `impl<T>
# Foo<T>` and `impl<T> Trait<T> for Foo<T>` tokenize down to plain
# identifiers instead of the generic's own type variables.
todo_context() {
    awk '
    {
        line = $0
        stripped_fn = line
        while (stripped_fn ~ /<[^<>]*>/) { sub(/<[^<>]*>/, "", stripped_fn) }
        n = split(stripped_fn, toks, /[^A-Za-z0-9_]+/)
        for (i = 1; i <= n; i++) {
            if (toks[i] == "fn" && i < n && toks[i + 1] != "") pending_fn = toks[i + 1]
        }
        if (line ~ /(^|[^A-Za-z0-9_])impl([^A-Za-z0-9_]|$)/) {
            stripped_impl = line
            while (stripped_impl ~ /<[^<>]*>/) { sub(/<[^<>]*>/, "", stripped_impl) }
            m = split(stripped_impl, itoks, /[^A-Za-z0-9_]+/)
            for (i = 1; i <= m; i++) {
                if (itoks[i] == "impl") {
                    typ = (i + 1 <= m) ? itoks[i + 1] : ""
                    if (i + 2 <= m && itoks[i + 2] == "for" && i + 3 <= m) typ = itoks[i + 3]
                    pending_impl = typ
                }
            }
        }
        len = length(line)
        for (k = 1; k <= len; k++) {
            c = substr(line, k, 1)
            if (c == "{") {
                depth++
                impl_stack[depth] = (pending_impl != "") ? pending_impl : impl_stack[depth - 1]
                fn_stack[depth]   = (pending_fn   != "") ? pending_fn   : fn_stack[depth - 1]
                pending_impl = ""
                pending_fn = ""
            } else if (c == "}") {
                delete impl_stack[depth]
                delete fn_stack[depth]
                if (depth > 0) depth--
            }
        }
        if (index(line, "todo!(") > 0) printf "%s\t%s\n", impl_stack[depth], fn_stack[depth]
    }
    ' "$1"
}

while IFS= read -r rs; do
    crate="$(echo "$rs" | cut -d/ -f2 | tr '-' '_')"
    while IFS=$'\t' read -r type method; do
        if [ -z "$type" ] || [ -z "$method" ]; then
            fail "$rs: todo!( site outside a recognizable impl/fn"
            continue
        fi
        todo_is_declared "$crate" "$type" "$method" ||
            fail "$rs: todo!( in ${crate}::${type}::${method} is not in CLAUDE.md's Known scaffolding list"
    done < <(todo_context "$rs")
done < <(grep -rl 'todo!(' crates --include='*.rs' || true)

if [ "$status" -eq 0 ]; then
    echo "check_docs: ok"
fi
exit "$status"
