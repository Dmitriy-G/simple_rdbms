# replacer

`Replacer`, the trait that chooses which buffer pool frame to evict when a
new page must be brought into memory and no frame is free, and
`LruKReplacer`, its LRU-K implementation.

## Key Components

- `Replacer` - implementations decide *which* evictable frame to reclaim;
  the buffer pool itself is responsible for only ever offering up frames
  whose pin count is zero. `record_access(&mut self, frame_id)` records
  that `frame_id` was just accessed, for recency/frequency bookkeeping.
  `set_evictable(&mut self, frame_id, evictable)` marks `frame_id` as
  evictable (pin count dropped to zero) or not (pin count rose above
  zero); it only ever modifies an entry that `record_access` has already
  created and is a no-op on any `frame_id` the replacer isn't tracking -
  a frame the replacer doesn't know about is either still owned by
  whichever caller is installing it, or sitting in the buffer pool's free
  list, and in both cases the replacer treating it as evictable would be
  wrong (`docs/adr/0009-buffer-pool-frame-ownership.md`). `evict(&mut self) -> Option<FrameId>` picks a victim frame to
  evict according to the replacement policy, and stops tracking it;
  returns `None` if no frame is evictable. `remove(&mut self, frame_id)`
  removes a frame from tracking entirely, e.g. because its page was
  deallocated. `size(&self) -> usize` is the number of frames currently
  marked evictable.
- `FrameEntry` - private. A frame's access history and evictability, as
  tracked by `LruKReplacer`: `history` (the frame's `k` most recent
  access timestamps, oldest first) and `evictable`.
- `LruKReplacer` - evicts the frame whose K-th most recent access lies
  furthest in the past, falling back to plain LRU (evict the least
  recently used) for frames with fewer than K recorded accesses. This
  approximates "least likely to be reused soon" better than plain LRU
  under sequential-scan workloads, which tend to defeat plain LRU.
  `LruKReplacer::new(capacity, k)` creates a replacer tracking up to
  `capacity` frames, ranking eviction candidates by their `k`-th most
  recent access. Its `Replacer::evict` ranks candidates as
  `(has_less_than_k_accesses, distance)`: frames with fewer than `k`
  accesses (infinite backward k-distance, so ranked purely by how stale
  their most recent access is) always beat frames with a full history
  (ranked by distance since their k-th most recent access); larger is a
  better eviction candidate.

## Usage Example

```rust
use storage::replacer::{LruKReplacer, Replacer};

let mut replacer = LruKReplacer::new(64, 2);
replacer.record_access(common::FrameId(3));
replacer.set_evictable(common::FrameId(3), true);
let victim = replacer.evict();
```

`BufferPool::allocate_frame` is the sole caller: it asks the configured
`Replacer` for a victim whenever the free list is empty, and calls
`record_access`/`set_evictable` from `pin`/`unpin_frame` on every
pin/unpin. The buffer pool is constructed with a `Box<dyn Replacer>`, so a
different eviction policy could be swapped in without touching
`BufferPool` itself.
