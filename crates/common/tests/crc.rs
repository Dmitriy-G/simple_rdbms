use common::crc::crc32;

#[test]
fn empty_input_is_zero() {
    assert_eq!(crc32(b""), 0x0000_0000);
}

#[test]
fn known_vector_123456789() {
    assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
}
