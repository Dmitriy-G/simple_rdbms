//! Fills a slotted page past capacity and checks every tuple inserted
//! before that point is still readable.

use common::PageId;
use storage::heap::SlottedPage;
use storage::page::Page;

#[test]
fn insert_returns_none_once_full_and_prior_tuples_stay_readable() {
    let mut page = Page::new(PageId(1));
    let mut slotted = SlottedPage::new(&mut page);
    slotted.init();

    let payload = [0xABu8; 200];
    let mut slots = Vec::new();
    while let Some(slot) = slotted.insert(&payload) {
        slots.push(slot);
    }

    assert!(!slots.is_empty(), "at least one 200-byte tuple should fit in a 4KiB page");

    for slot in slots {
        assert_eq!(slotted.read(slot), Some(payload.as_slice()));
    }
}

#[test]
fn deleted_slot_reads_as_none_but_others_survive() {
    let mut page = Page::new(PageId(1));
    let mut slotted = SlottedPage::new(&mut page);
    slotted.init();

    let Some(a) = slotted.insert(b"first") else {
        panic!("first tuple should fit in an empty page");
    };
    let Some(b) = slotted.insert(b"second") else {
        panic!("second tuple should fit in an empty page");
    };

    slotted.delete(a);

    assert_eq!(slotted.read(a), None);
    assert_eq!(slotted.read(b), Some(b"second".as_slice()));
}
