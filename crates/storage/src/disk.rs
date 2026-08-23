use std::fs::OpenOptions;
use std::path::PathBuf;

use common::PageId;

use crate::block_device::{BlockDevice, FileDevice};
use crate::error::StorageError;
use crate::page::Page;

/// The 8-byte magic string identifying a FerroDB database file, stored at
/// the start of page 0 - just after page 0's own `page_lsn` prefix (see
/// `header::MAGIC_RANGE`).
const MAGIC: &[u8; 8] = b"FERRODB\0";

/// The on-disk format version this build reads and writes. Covers not just
/// the page-0 header's own byte layout but the heap page layout too (see
/// `heap::NO_NEXT_PAGE` and `heap::SlottedPage::init`): both are baked into
/// every page already on disk, so a change to either must bump this,
/// otherwise a file written by an older build gets silently misread rather
/// than rejected with the clear error below. Bumped to `4` when the header
/// stopped storing `page_count`: `next_page_id` is now derived from the
/// file's actual length at open time (see `DiskManager::open`), so
/// `allocate_page`'s `set_len` is the single durable act of allocation
/// instead of racing a separate header rewrite against the new page's own
/// initialization. Bumped to `5` when every page gained an 8-byte
/// `page_lsn` prefix (see `page::PAGE_LSN_RANGE`), which pushed the
/// slotted-page header from `0..8` to `8..16` and shrank `MAX_SLOTS` and
/// `MAX_TUPLE_SIZE` accordingly - a file written under version `4` has no
/// `page_lsn` and would otherwise be misread as heap corruption. Bumped to
/// `6` when the header gained `last_checkpoint_lsn` (see
/// `header::LAST_CHECKPOINT_LSN_RANGE`), which recovery's Analysis pass
/// reads to know where to start scanning the log. Bumped to `7` when page 0
/// gained the same 8-byte `page_lsn` prefix every other page already has,
/// shifting every other field 8 bytes later - needed so `catalog_first_page`
/// and `last_checkpoint_lsn` can be mutated through the ordinary
/// `PageGuard::write` path (see `buffer::BufferPool::set_catalog_first_page`/
/// `set_last_checkpoint_lsn`) instead of a raw, unlogged disk write. No
/// migration: a file written under an older version must be deleted and
/// recreated.
const HEADER_VERSION: u32 = 7;

/// The byte layout of the page-0 header: `page_lsn` (8, reserved by every
/// page - see `page::PAGE_LSN_RANGE`) | magic (8) | version (4) |
/// catalog_first_page (4) | page_size (4) | last_checkpoint_lsn (8),
/// zero-padded to a full page. `page_count` is deliberately not part of
/// this layout - see `HEADER_VERSION`'s doc comment.
///
/// `catalog_first_page` and `last_checkpoint_lsn` are mutated exclusively
/// through `buffer::BufferPool` once the pool exists - fetching page 0 like
/// any other page and writing through `PageGuard::write`, so both become
/// redoable and undoable like any other page mutation. `DiskManager` itself
/// only ever *reads* this layout (see `open_with_device`), and writes it
/// directly, in full, exactly once: stamping a brand-new file's very first
/// header in `write_header`, before there is any pool or log to write
/// through.
pub(crate) mod header {
    pub const MAGIC_RANGE: std::ops::Range<usize> = 8..16;
    pub const VERSION_RANGE: std::ops::Range<usize> = 16..20;
    pub const CATALOG_FIRST_PAGE_RANGE: std::ops::Range<usize> = 20..24;
    pub const PAGE_SIZE_RANGE: std::ops::Range<usize> = 24..28;
    pub const LAST_CHECKPOINT_LSN_RANGE: std::ops::Range<usize> = 28..36;
}

/// Owns the database file and performs raw, page-granular I/O against it.
/// This is the only module allowed to call into `std::fs` for the main
/// database file; every layer above (buffer pool, heap, B+tree) reads and
/// writes pages through here rather than touching the file directly.
pub struct DiskManager {
    device: Box<dyn BlockDevice>,
    #[allow(dead_code)]
    path: Option<PathBuf>,
    page_size: usize,
    next_page_id: u32,
}

impl DiskManager {
    /// Opens (creating if necessary) the database file at `path`, using
    /// `page_size`-byte pages.
    pub fn open(path: impl Into<PathBuf>, page_size: usize) -> Result<Self, StorageError> {
        let path = path.into();
        let file =
            OpenOptions::new().read(true).write(true).create(true).truncate(false).open(&path)?;
        Self::open_with_device(Box::new(FileDevice::new(file)), page_size, Some(path))
    }

    /// Opens a database file backed by an arbitrary `BlockDevice`, for
    /// tests and crash-injection fault wrapping. A brand-new (zero-length)
    /// device gets a fresh page-0 header written to it; an existing one has
    /// its header validated (magic and version) and `next_page_id` derived
    /// from the device's own length - `device_len / page_size` - rather
    /// than from any stored count, so the device's length is the single
    /// source of truth for how many pages have been durably allocated.
    pub fn open_with_device(
        mut device: Box<dyn BlockDevice>,
        page_size: usize,
        path: Option<PathBuf>,
    ) -> Result<Self, StorageError> {
        let device_len = device.size()?;

        if device_len == 0 {
            let mut manager = Self { device, path, page_size, next_page_id: 1 };
            manager.write_header()?;
            Ok(manager)
        } else {
            if device_len % page_size as u64 != 0 {
                let whole_pages = device_len / page_size as u64;
                return Err(StorageError::TruncatedFile {
                    actual: device_len,
                    expected: whole_pages * page_size as u64,
                    page_size,
                });
            }

            // Read only the header fields `DiskManager` itself still cares
            // about (magic, version, page size) - not a full `page_size`
            // bytes, and not `catalog_first_page`/`last_checkpoint_lsn`
            // either, since those are read live through the buffer pool now
            // (see `buffer::BufferPool::catalog_first_page`/
            // `last_checkpoint_lsn`), not cached here.
            let mut header_buf = vec![0u8; header::PAGE_SIZE_RANGE.end];
            device.read_at(0, &mut header_buf)?;

            if &header_buf[header::MAGIC_RANGE] != MAGIC.as_slice() {
                return Err(StorageError::CorruptPage {
                    page_id: 0,
                    reason: "bad magic in database header".to_string(),
                });
            }
            let version = read_u32(&header_buf, header::VERSION_RANGE.start);
            if version != HEADER_VERSION {
                return Err(StorageError::CorruptPage {
                    page_id: 0,
                    reason: format!(
                        "unsupported on-disk format version {version}: this build reads and \
                         writes version {HEADER_VERSION}"
                    ),
                });
            }
            let stored_page_size = read_u32(&header_buf, header::PAGE_SIZE_RANGE.start);
            if stored_page_size as usize != page_size {
                return Err(StorageError::CorruptPage {
                    page_id: 0,
                    reason: format!(
                        "page size mismatch: database was created with page_size \
                         {stored_page_size}, but {page_size} was requested"
                    ),
                });
            }

            let next_page_id = (device_len / page_size as u64) as u32;
            Ok(Self { device, path, page_size, next_page_id })
        }
    }

    /// Reads the page at `page_id` from disk into `page`. Reading a page
    /// that was never allocated (at or past the file's current extent) is
    /// an error rather than yielding zeroed bytes.
    pub fn read_page(&mut self, page_id: PageId, page: &mut Page) -> Result<(), StorageError> {
        let offset = self.offset_of(page_id);
        let device_len = self.device.size()?;
        if offset + self.page_size as u64 > device_len {
            return Err(StorageError::PageNotFound(page_id.0));
        }
        self.device.read_at(offset, page.data_mut())?;
        Ok(())
    }

    /// Writes `page`'s contents to its slot on disk.
    pub fn write_page(&mut self, page_id: PageId, page: &Page) -> Result<(), StorageError> {
        let offset = self.offset_of(page_id);
        self.device.write_at(offset, page.data())?;
        Ok(())
    }

    /// Allocates a new page id by extending the file, without writing any
    /// tuple data to it yet. `set_len` is the only durable act of
    /// allocation - a reopen derives `next_page_id` straight back from the
    /// file's length (see `open`), so there is no separate header write to
    /// race against the new page's own initialization, and no 4KB header
    /// rewrite on every allocation either.
    pub fn allocate_page(&mut self) -> Result<PageId, StorageError> {
        let page_id = PageId(self.next_page_id);
        self.next_page_id += 1;
        let new_len = self.next_page_id as u64 * self.page_size as u64;
        self.device.set_len(new_len)?;
        Ok(page_id)
    }

    /// Extends the file so that `page_id` is allocated, if it is not
    /// already - i.e. redoes an `AllocPage` record. Idempotent: a no-op if
    /// `page_id` is already within the file's current extent. Unlike
    /// `allocate_page`, this never assigns a *new* id; it only ensures a
    /// specific, already-decided one physically exists on disk.
    pub fn ensure_allocated(&mut self, page_id: PageId) -> Result<(), StorageError> {
        if page_id.0 >= self.next_page_id {
            self.next_page_id = page_id.0 + 1;
            let new_len = self.next_page_id as u64 * self.page_size as u64;
            self.device.set_len(new_len)?;
        }
        Ok(())
    }

    /// Forces every write made so far to durable storage.
    pub fn sync(&mut self) -> Result<(), StorageError> {
        self.device.sync_all()?;
        Ok(())
    }

    /// The configured page size for this database file.
    pub fn page_size(&self) -> usize {
        self.page_size
    }

    fn offset_of(&self, page_id: PageId) -> u64 {
        page_id.0 as u64 * self.page_size as u64
    }

    /// Stamps a brand-new file's very first page-0 header: magic, version,
    /// page size, and `catalog_first_page`/`last_checkpoint_lsn` both at
    /// their "none" sentinels (`page_lsn`, at the front, stays zero too -
    /// the same "never written" sentinel every other fresh page has). The
    /// only direct header write left in this module - every later change to
    /// `catalog_first_page` or `last_checkpoint_lsn` goes through
    /// `buffer::BufferPool` instead, so it is logged and can be redone or
    /// undone like any other page mutation.
    fn write_header(&mut self) -> Result<(), StorageError> {
        let mut buf = vec![0u8; self.page_size];
        buf[header::MAGIC_RANGE].copy_from_slice(MAGIC);
        buf[header::VERSION_RANGE].copy_from_slice(&HEADER_VERSION.to_le_bytes());
        buf[header::CATALOG_FIRST_PAGE_RANGE].copy_from_slice(&u32::MAX.to_le_bytes());
        buf[header::PAGE_SIZE_RANGE].copy_from_slice(&(self.page_size as u32).to_le_bytes());

        self.device.write_at(0, &buf)?;
        Ok(())
    }
}

fn read_u32(buf: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([buf[at], buf[at + 1], buf[at + 2], buf[at + 3]])
}
