# sync

A single poisoned-lock recovery helper, shared by every layer that
synchronizes shared state with `std::sync::{Mutex, RwLock}` rather than
`parking_lot` (see `crates/storage/src/buffer.MD` for why `std::sync` was
chosen). A `std::sync` lock poisons itself the moment a thread panics
while holding it, and every later `.lock()`/`.read()`/`.write()` on that
lock then returns `Err` forever — for a long-lived database process, that
would turn one bad statement's panic into a permanently unusable buffer
pool, catalog, or log. Recovering the guard via `PoisonError::into_inner`
instead accepts that the protected state might be left mid-update by the
panicking holder (true either way — poisoning doesn't undo the partial
write, it only remembers that one happened), but keeps every later,
unrelated operation working rather than wedging the whole process. Kept
in `common` rather than duplicated per crate since both `storage` (the
buffer pool, disk manager, log manager, double-write buffer) and `engine`
(the transaction manager) need it, and `common` is the one crate both
depend on.

## Key Components

- `recover_lock(result, what) -> T` - given the `LockResult<T>` a
  `Mutex`/`RwLock` lock call returns, unwraps it, recovering a poisoned
  lock's guard via `PoisonError::into_inner` rather than propagating the
  poisoning or panicking. `what` names the lock in the `error!` event
  logged when recovery actually happens (poisoning is expected to be
  vanishingly rare — a caller name, not a format string, is enough to
  find it). Generic over `T` (the guard type) since `Mutex::lock`,
  `RwLock::read`, and `RwLock::write` all return the same
  `Result<Guard, PoisonError<Guard>>` shape.

## Usage Example

```rust
use common::sync::recover_lock;
use std::sync::Mutex;

let state = Mutex::new(0);
let mut guard = recover_lock(state.lock(), "example.state");
*guard += 1;
```

Every `Mutex`/`RwLock` acquisition below `engine` goes through this
helper instead of `.unwrap()`/`.expect()` on the lock call directly (both
denied by `clippy::unwrap_used`/`clippy::expect_used` outside tests
anyway) - see `storage::buffer.MD`'s latch-ordering rule for where the
locks this helper unwraps actually live.
