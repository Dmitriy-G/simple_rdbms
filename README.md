# simple rdbms

A relational database engine written from scratch in Rust: its own page
format, buffer pool, B+tree, write-ahead log, SQL lexer/parser, planner,
and Volcano-style executor. No `sqlparser`, no embedded storage engine, no
async runtime — synchronous, single-node, and hand-written end to end.

This is a learning project. `CREATE TABLE`/`INSERT`/`SELECT` work end to
end — SQL text in, rows out, durable across a restart — but a database has
no indexes yet, no crash recovery, and no concurrency: everything past a
sequential scan under a single implicit transaction is still `todo!()`. See
`docs/ROADMAP.md` for the order the rest gets built in.

## Build & run

```sh
cargo build
cargo run -p cli
```

The REPL opens (creating if needed) a database file — pass a path as the
first argument, or it defaults to `simple_rdbms.db` in the current
directory — and reads SQL statements from stdin, buffering lines until a
`;` terminates one. Meta commands (`.tables`, `.schema <name>`, `.exit`)
take effect immediately, without a `;`. A query error prints and returns to
the prompt rather than ending the session; only `.exit` or EOF does that.

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

## REPL demo

A real transcript (`cargo run -p cli -- /tmp/demo.db`), showing a
multi-line statement, `NULL` handling, an error that returns to the prompt
instead of exiting, and the meta commands:

```
simple_rdbms> CREATE TABLE users (id INTEGER, name TEXT, age INTEGER);
OK
simple_rdbms> INSERT INTO users (id, name, age) VALUES (1, 'Alice', 30), (2, 'Bob', 25), (3, 'Carol', NULL);
OK (3 rows)
simple_rdbms> SELECT * FROM users;
id | name  | age 
---+-------+-----
1  | Alice | 30  
2  | Bob   | 25  
3  | Carol | NULL
simple_rdbms> SELECT name, age FROM users WHERE age < 28;
name | age
-----+----
Bob  | 25 
simple_rdbms> SELECT name FROM users WHERE age > 20 AND age < 40 OR name = 'Carol';
name 
-----
Alice
Bob  
Carol
simple_rdbms> SELECT * FROM ghosts;
error: binder error: unknown table: ghosts
simple_rdbms> .tables
users
simple_rdbms> .schema users
id INTEGER
name TEXT
age INTEGER
simple_rdbms> .exit
```

Note the last query: `age > 20 AND age < 40 OR name = 'Carol'` matches
Alice and Bob on the first clause, and Carol on the second — even though
Carol's `age` is `NULL` and `age > 20 AND age < 40` evaluates to `NULL`
(not `false`) for her row, three-valued `OR` still resolves to `true`
because the other operand is definitely `true`.

## Docs

- `docs/ROADMAP.md` — milestones, framed as the database problem each one
  solves.
- `docs/adr/` — architecture decision records.
- `CLAUDE.md` — commands, conventions, and the dependency-edge rules for
  anyone (human or agent) working in this repo.
