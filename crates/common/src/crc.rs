//! A hand-written, table-driven CRC-32 (the IEEE 802.3 polynomial, reflected
//! form — the same checksum used by zip, gzip, and Ethernet), used by the
//! write-ahead log to detect a torn or corrupted record. No external crate:
//! the table is built once, lazily, from the polynomial itself.

use std::sync::OnceLock;

/// The reflected IEEE 802.3 polynomial, `0xEDB88320` (the bit-reversal of
/// the standard `0x04C11DB7` polynomial), used because CRC-32 is
/// conventionally computed least-significant-bit first.
const POLYNOMIAL: u32 = 0xEDB8_8320;

fn table() -> &'static [u32; 256] {
    static TABLE: OnceLock<[u32; 256]> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut table = [0u32; 256];
        let mut byte = 0u32;
        while byte < 256 {
            let mut crc = byte;
            let mut bit = 0;
            while bit < 8 {
                crc = if crc & 1 != 0 { POLYNOMIAL ^ (crc >> 1) } else { crc >> 1 };
                bit += 1;
            }
            table[byte as usize] = crc;
            byte += 1;
        }
        table
    })
}

/// Computes the CRC-32 (IEEE 802.3) checksum of `data`.
pub fn crc32(data: &[u8]) -> u32 {
    let table = table();
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in data {
        crc = table[((crc ^ byte as u32) & 0xFF) as usize] ^ (crc >> 8);
    }
    crc ^ 0xFFFF_FFFF
}

#[cfg(test)]
mod tests {
    use super::crc32;

    #[test]
    fn empty_input_is_zero() {
        assert_eq!(crc32(b""), 0x0000_0000);
    }

    #[test]
    fn known_vector_123456789() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }
}
