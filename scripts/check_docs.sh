#!/usr/bin/env bash
# Enforces the sibling-.MD documentation convention from CLAUDE.md:
#   - every crates/**/*.rs file has a sibling <stem>.MD beside it
#   - every crates/**/*.MD file has a sibling <stem>.rs beside it
#   - every such .MD file has a "## Key Components" heading
#   - no such .rs file has a `///` or `//!` doc comment, or a `//` comment
#     that isn't `// SAFETY:` or `// TODO(`
#
# Scoped per crate, not across the whole crates/ tree at once: a crate
# directory only comes under enforcement once it already contains at least
# one .MD file somewhere inside it. This is what lets the migration proceed
# one crate at a time (see CLAUDE.md's Documentation section) without CI
# going red for the ~90 files nobody has converted yet. Once every crate has
# at least one .MD file, every crate is in scope and this is equivalent to
# checking the whole workspace at once, which is the steady-state rule.
set -euo pipefail
cd "$(dirname "$0")/.."

status=0

fail() {
    echo "check_docs: $1" >&2
    status=1
}

for crate_dir in crates/*/; do
    crate_dir=${crate_dir%/}
    if [ -z "$(find "$crate_dir" -name '*.MD' -print -quit)" ]; then
        continue
    fi

    while IFS= read -r rs; do
        md="${rs%.rs}.MD"
        [ -f "$md" ] || fail "$rs has no sibling $md"
    done < <(find "$crate_dir" -name '*.rs')

    while IFS= read -r md; do
        rs="${md%.MD}.rs"
        [ -f "$rs" ] || fail "$md has no sibling $rs"
        grep -q '^## Key Components$' "$md" || fail "$md is missing the '## Key Components' heading"
    done < <(find "$crate_dir" -name '*.MD')

    while IFS= read -r rs; do
        while IFS= read -r comment; do
            case "$comment" in
                '// SAFETY:'*|'// TODO('*) ;;
                *) fail "$rs: disallowed comment: $comment" ;;
            esac
        done < <(grep -oE '(^|[[:space:]])//.*$' "$rs" | sed -E 's/^[[:space:]]+//')
    done < <(find "$crate_dir" -name '*.rs')
done

if [ "$status" -eq 0 ]; then
    echo "check_docs: ok"
fi
exit "$status"
