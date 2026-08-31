use storage::page::{PAGE_SIZE, checksum_of, checksum_ok, stamp_checksum};

#[test]
fn an_untouched_all_zero_page_reads_as_checksum_ok() {
    let bytes = [0u8; PAGE_SIZE];
    assert!(checksum_ok(&bytes), "an all-zero page must decode as valid, not corrupt");
}

#[test]
fn a_freshly_stamped_page_reads_as_checksum_ok() {
    let mut bytes = [0u8; PAGE_SIZE];
    bytes[100] = 0xAB;
    stamp_checksum(&mut bytes);
    assert!(checksum_ok(&bytes));
}

#[test]
fn a_flipped_bit_anywhere_in_the_payload_fails_the_checksum() {
    let mut bytes = [0u8; PAGE_SIZE];
    bytes[100] = 0xAB;
    stamp_checksum(&mut bytes);

    bytes[100] ^= 0x01;
    assert!(!checksum_ok(&bytes));
}

#[test]
fn a_flipped_bit_in_the_stored_checksum_itself_fails_the_checksum() {
    let mut bytes = [0u8; PAGE_SIZE];
    bytes[100] = 0xAB;
    stamp_checksum(&mut bytes);

    bytes[0] ^= 0x01;
    assert!(!checksum_ok(&bytes));
}

#[test]
fn checksum_of_ignores_the_checksum_field_itself() {
    let mut bytes = [0u8; PAGE_SIZE];
    bytes[100] = 0xAB;
    let before = checksum_of(&bytes);

    bytes[0] = 0xFF;
    bytes[1] = 0xFF;
    bytes[2] = 0xFF;
    bytes[3] = 0xFF;
    let after = checksum_of(&bytes);

    assert_eq!(before, after, "the checksum field itself must not be covered by its own CRC");
}
