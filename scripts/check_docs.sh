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

if [ "$status" -eq 0 ]; then
    echo "check_docs: ok"
fi
exit "$status"
