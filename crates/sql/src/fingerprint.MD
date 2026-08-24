# fingerprint

Redacts a SQL statement's literal values so it's safe to log at `info` -
see CLAUDE.md's logging section: full SQL text is a `debug`-and-below
concern, but a normalized shape of *which* statement ran is worth an
`info` line, and the only way to get that without leaking whatever a user
typed into a `VALUES` clause is to strip the literals out first.

Reuses the lexer rather than a separate regex or hand-rolled scanner:
`Lexer::tokenize` already knows exactly where every token starts (`Token::offset`)
and which ones are literals (`IntegerLiteral`/`FloatLiteral`/`StringLiteral`),
so this module never has to re-derive "what counts as a number" or "how do
quotes and escapes work" - it trusts the same boundaries the parser itself
trusts.

## Key Components

- `fingerprint(sql) -> String` - copies `sql` back out character for
  character, except every literal token's span is replaced with a single
  `?`. Everything else - keywords, identifiers, punctuation, and all
  original whitespace between tokens - is preserved verbatim, so
  `INSERT INTO t VALUES (42, 'alice')` becomes
  `INSERT INTO t VALUES (?, ?)`. A token's end isn't tracked directly, so
  it's derived as the next token's start with trailing whitespace trimmed
  off - safe because the lexer skips only whitespace between tokens, never
  other characters. If `sql` fails to lex at all, the whole thing might be
  an unterminated string literal (i.e. arbitrary user text with no lexical
  structure to redact around), so this returns a single `"?"` rather than
  risk leaking a fragment.

## Usage Example

```rust
use sql::fingerprint;

assert_eq!(
    fingerprint("INSERT INTO t VALUES (42, 'alice')"),
    "INSERT INTO t VALUES (?, ?)"
);
assert_eq!(fingerprint("SELECT * FROM t WHERE a = 1"), "SELECT * FROM t WHERE a = ?");
```

`engine::Database::execute` calls this once per statement to build the
`info`-level "statement executed" event's `statement` field; the
un-redacted SQL text only ever appears in the `debug`-level event
alongside it, never above.
