# slotted_page

Fills a slotted page past capacity and checks every tuple inserted before
that point is still readable.

## Key Components

- `TXN` - `TxnId(0)`, the fixed transaction id these tests write under.
- `open_pool()` - opens a 4-frame `BufferPool` over a fresh temp-dir
  database.
- `insert_returns_none_once_full_and_prior_tuples_stay_readable` - inserts
  200-byte tuples until `insert` returns `None`, then reads every
  previously-inserted slot back and checks its bytes are unchanged.
- `deleted_slot_reads_as_none_but_others_survive` - inserts two tuples,
  deletes the first, and checks the deleted slot reads as `None` while
  the second tuple is untouched.

## Usage Example

```rust
let (pool, _dir) = open_pool()?;
let (_page_id, mut guard) = pool.new_page(TXN)?;
let mut slotted = SlottedPage::new(&mut guard, TXN);
slotted.init()?;
let slot = slotted.insert(b"hello")?.expect("should fit in an empty page");
assert_eq!(slotted.read(slot)?, Some(b"hello".as_slice()));
```

Run via `cargo test -p storage --test slotted_page`, driving
`storage::heap::SlottedPage` directly against a single page rather than
through `TableHeap`'s multi-page chaining (see `table_heap.rs` for that).
