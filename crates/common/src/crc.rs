use std::sync::OnceLock;

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

pub fn crc32(data: &[u8]) -> u32 {
    let table = table();
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in data {
        crc = table[((crc ^ byte as u32) & 0xFF) as usize] ^ (crc >> 8);
    }
    crc ^ 0xFFFF_FFFF
}
