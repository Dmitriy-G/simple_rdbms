# CLAUDE.md

Guidance for anyone — human or agent — working in this repository.

## Commands

Build:
```sh
cargo build
```

Run the REPL:
```sh
cargo run -p cli -- [db_path]
```

Format:
```sh
cargo fmt --all              # apply
cargo fmt --all -- --check   # verify, as CI does
```

Lint (must be warning-free):
```sh
cargo clippy --workspace --all-targets -- -D warnings
```

Test:
```sh
cargo test --workspace
```

All four (`fmt --check`, `clippy -D warnings`, `build`, `test`) must pass
on a clean checkout; CI (`.github/workflows/ci.yml`) runs the same commands
on every PR.

## Dependency-edge rules

The workspace is split into layered crates under `crates/`, and the
allowed dependency edges between them are fixed:

```
common   -> (none)
types    -> common
storage  -> common, types
catalog  -> common, types, storage
sql      -> common, types
txn      -> common, storage
planner  -> common, types, catalog, sql
executor -> common, types, catalog, storage, txn, planner
engine   -> all of the above
cli      -> engine, common
```

No cycles, no back-edges, and no edge beyond this list. This is enforced
by the compiler (a disallowed `path`/`workspace` dependency simply won't
build), not by convention — see `docs/adr/0002-crate-splitting.md` for why.
If a crate seems to need a dependency not on this list, that's a sign the
change belongs in a different crate, or the edge list itself needs an ADR
updating it — don't just add the dependency.

## Commit and branch conventions

Commits follow [Conventional Commits](https://www.conventionalcommits.org/):

```
<type>(<scope>): <summary>
```

`type` is one of `feat`, `fix`, `refactor`, `test`, `docs`, `chore`,
`perf`, `build`, `ci`. `scope` is normally a crate name (`storage`,
`planner`, ...) or `workspace` for changes spanning crates.

Branches are named `<type>/<kebab-description>`, e.g.
`feat/buffer-pool-eviction` or `fix/slotted-page-overflow`.

## Testing rules

Tests must be deterministic: no assertions that depend on wall-clock time,
hash map/set iteration order, or thread scheduling. Use `tempfile` for
anything touching the filesystem so tests don't collide or leave state
behind, and seed any randomness (`proptest` included) rather than relying
on ambient entropy.
