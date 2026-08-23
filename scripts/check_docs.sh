#!/usr/bin/env bash
# Enforces the sibling-.MD documentation convention from CLAUDE.md, across
# the whole crates/ tree:
#   - every crates/**/*.rs file has a sibling <stem>.MD beside it
#   - every crates/**/*.MD file has a sibling <stem>.rs beside it
#   - every such .MD file has a "## Key Components" heading
#   - no such .rs file has a `///` or `//!` doc comment, or a `//` comment
#     that isn't `// SAFETY:` or `// TODO(`
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

while IFS= read -r rs; do
    while IFS= read -r comment; do
        case "$comment" in
            '// SAFETY:'*|'// TODO('*) ;;
            *) fail "$rs: disallowed comment: $comment" ;;
        esac
    done < <(grep -oE '(^|[[:space:]])//.*$' "$rs" | sed -E 's/^[[:space:]]+//')
done < <(find crates -name '*.rs')

if [ "$status" -eq 0 ]; then
    echo "check_docs: ok"
fi
exit "$status"
