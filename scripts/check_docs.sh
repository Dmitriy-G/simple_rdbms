#!/usr/bin/env bash
# Enforces the sibling-.MD documentation convention from CLAUDE.md, across
# the whole crates/ tree:
#   - every crates/**/*.rs file has a sibling <stem>.MD beside it
#   - every crates/**/*.MD file has a sibling <stem>.rs beside it
#   - every such .MD file has a "## Key Components" heading
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
    grep -q '^## Key Components$' "$md" || fail "$md is missing the '## Key Components' heading"
done < <(find crates -name '*.MD')

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
