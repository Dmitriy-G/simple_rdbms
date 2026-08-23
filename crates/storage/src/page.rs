use common::{FrameId, Lsn, PageId};

/// The fixed size, in bytes, of every page in the database file and the
/// write-ahead log. Matches `common::DbConfig::DEFAULT_PAGE_SIZE`; a
/// database file's actual page size is recorded in the page-0 header at
/// creation time, and `DiskManager::open` rejects a reopen whose requested
/// page size disagrees with it, so it is not meant to vary per-page or
/// across reopens of the same file.
pub const PAGE_SIZE: usize = 4096;

/// The byte range, at the very start of every page, reserved for a CRC-32
/// checksum over the rest of the page's bytes (`4..PAGE_SIZE`). Computed and
/// verified at the `DiskManager` layer (`write_page`/`read_page`), not by
/// any layout built on top of `Page`, so it is format-agnostic: the slotted
/// heap page format gets it today, and the B+tree's node pages will get it
/// for free once M9 adds them, with no changes to that code. See
/// `disk::DiskManager::read_page`'s doc comment for why an all-zero page
/// skips verification instead of failing it.
pub const CHECKSUM_RANGE: std::ops::Range<usize> = 0..4;

/// The byte range, just after `CHECKSUM_RANGE`, reserved for the page's
/// `pageLSN`: the log sequence number of the last WAL record that
/// describes a change to this page's bytes. The write-ahead rule is
/// enforced by comparing this against the log's own durable LSN before the
/// page is allowed to reach disk (see `BufferPool::flush_frame`), so every
/// page layout built on top of `Page` (slotted heap pages, B+tree nodes)
/// must treat these 8 bytes, together with `CHECKSUM_RANGE`, as reserved,
/// not available for its own layout.
pub const PAGE_LSN_RANGE: std::ops::Range<usize> = 4..12;

/// A single fixed-size page: the unit of I/O between the disk manager and
/// the buffer pool, and the unit of layout for slotted-page heap storage
/// and B+tree nodes. Holds raw bytes; interpreting them (as a heap page, an
/// index node, etc.) is the job of the module that owns that layout.
#[derive(Clone)]
pub struct Page {
    id: PageId,
    data: [u8; PAGE_SIZE],
}

impl Page {
    /// Creates a zeroed page for the given id, as when extending the file
    /// with a brand-new page.
    pub fn new(id: PageId) -> Self {
        Self { id, data: [0u8; PAGE_SIZE] }
    }

    /// The id of this page within the database file.
    pub fn id(&self) -> PageId {
        self.id
    }

    /// Read-only access to the page's raw bytes.
    pub fn data(&self) -> &[u8; PAGE_SIZE] {
        &self.data
    }

    /// Mutable access to the page's raw bytes, for in-place layout updates.
    /// Not exposed outside the crate as a way to mutate a *resident* page's
    /// bytes — `PageGuard::write` is the only path for that, since it is
    /// what logs the change before applying it. This stays `pub` for
    /// callers building a `Page` from scratch off the buffer pool (e.g.
    /// `DiskManager::read_page`, which fills a freshly constructed page
    /// from disk with nothing yet to log).
    pub fn data_mut(&mut self) -> &mut [u8; PAGE_SIZE] {
        &mut self.data
    }

    /// The LSN of the last WAL record describing a change to this page, or
    /// `Lsn(0)` if the page has never been written through
    /// `PageGuard::write` (a fresh, all-zero page).
    pub fn page_lsn(&self) -> Lsn {
        let bytes = &self.data[PAGE_LSN_RANGE];
        Lsn(u64::from_le_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ]))
    }

    /// Stamps this page with `lsn`, recording that its bytes now reflect
    /// every WAL record up to and including `lsn`. Called by
    /// `PageGuard::write` after applying a change, never directly by
    /// higher layers.
    pub fn set_page_lsn(&mut self, lsn: Lsn) {
        self.data[PAGE_LSN_RANGE].copy_from_slice(&lsn.0.to_le_bytes());
    }
}

/// An RAII handle to a page pinned in the buffer pool. Borrowing a page
/// through a `PageGuard` (rather than a bare `&Page`) is what lets the
/// buffer pool track pin counts: acquiring a guard increments the frame's
/// pin count, and dropping it decrements that count, making the frame
/// eligible for eviction again once nothing holds a guard to it.
///
/// `PageGuard` is the *only* way to release a pin: it has no public
/// constructor (its fields are `pub(crate)`) and `BufferPool` exposes no
/// manual unpin method, so the sole way to release a pin is to drop the
/// guard that holds it. A caller cannot double-release a pin, because
/// there is no second operation left to call:
///
/// ```compile_fail
/// # use common::TxnId;
/// # use storage::buffer::BufferPool;
/// # use storage::disk::DiskManager;
/// # use storage::replacer::LruKReplacer;
/// # use storage::wal::LogManager;
/// # let dir = tempfile::tempdir().unwrap();
/// # let disk = DiskManager::open(dir.path().join("t.db"), storage::page::PAGE_SIZE).unwrap();
/// # let log = LogManager::open(dir.path().join("t.db.wal")).unwrap();
/// # let pool = BufferPool::new(disk, log, 4, Box::new(LruKReplacer::new(4, 2)));
/// let (page_id, guard) = pool.new_page(TxnId(0)).unwrap();
/// drop(guard);
/// pool.unpin_page(page_id, false).unwrap(); // no such method on `BufferPool`
/// ```
pub struct PageGuard<'pool> {
    pub(crate) page_id: PageId,
    /// The frame in the pool's frame table currently holding this page.
    pub(crate) frame_id: FrameId,
    pub(crate) pool: &'pool crate::buffer::BufferPool,
}

impl<'pool> PageGuard<'pool> {
    /// The id of the page this guard pins.
    pub fn page_id(&self) -> PageId {
        self.page_id
    }
}

// `page()`, `write()`, and `Drop` live in `crate::buffer` alongside
// `BufferPool`, since they need access to its private frame table, pin
// counts, dirty flags, and log manager.
