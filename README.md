# simple rdbms

A relational database engine written from scratch in Rust: its own page
format, buffer pool, B+tree, write-ahead log, SQL lexer/parser, planner,
and Volcano-style executor. No `sqlparser`, no embedded storage engine, no
async runtime — synchronous, single-node, and hand-written end to end.

This is a learning project, currently at the scaffolding stage: crate
boundaries, public types, and signatures exist; most function bodies are
`todo!()`. See `docs/ROADMAP.md` for the order features will actually get
implemented in.

## Build & run

```sh
cargo build
cargo run -p cli
```

The REPL opens (creating if needed) a database file — pass a path as the
first argument, or it defaults to `simple_rdbms.db` in the current
directory — and reads SQL statements from stdin, one per line, until
`exit`/`quit` or EOF.

## Test & lint

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

## Crate map

| Crate | Responsibility |
| --- | --- |
| [`common`](crates/common) | Shared error type, `Result` alias, newtype ids, top-level config. |
| [`types`](crates/types) | `DataType`/`Value`, null handling, comparison, tuple encode/decode. |
| [`storage`](crates/storage) | Disk manager, buffer pool, slotted-page heap files, B+tree, WAL. |
| [`catalog`](crates/catalog) | Table/column metadata, in-memory for now. |
| [`sql`](crates/sql) | Hand-written lexer and recursive-descent parser, AST types. |
| [`txn`](crates/txn) | Transaction lifecycle, lock manager, isolation levels, MVCC types. |
| [`planner`](crates/planner) | Binder, logical/physical plans, optimizer rule trait. |
| [`executor`](crates/executor) | Volcano-style pull operators over a physical plan. |
| [`engine`](crates/engine) | `Database` facade: wires every layer together behind `execute(sql)`. |
| [`cli`](crates/cli) | The only binary: a REPL over `engine`. |

See `docs/diagrams/crate-dependencies.mmd` for the dependency graph, and
`docs/adr/0002-crate-splitting.md` for why the layers are crates rather
than modules.

## Docs

- `docs/ROADMAP.md` — milestones, framed as the database problem each one
  solves.
- `docs/adr/` — architecture decision records.
- `CLAUDE.md` — commands, conventions, and the dependency-edge rules for
  anyone (human or agent) working in this repo.
