/// CRC-16/MODBUS: reflected poly 0x8005 (0xA001), init 0xFFFF, no final XOR.
///
/// Transmitted little-endian (low byte first) after the payload.
pub fn crc16_modbus(data: &[u8]) -> u16 {
    let mut crc: u16 = 0xFFFF;
    for &b in data {
        crc ^= u16::from(b);
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xA001 } else { crc >> 1 };
        }
    }
    crc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_value() {
        assert_eq!(crc16_modbus(b"123456789"), 0x4B37);
    }

    #[test]
    fn wire_vectors() {
        // All observed on the wire (see docs/PROTOCOL.md §2).
        for (frame, lo, hi) in [
            (&[0xFB, 0x10, 0x00, 0x00][..], 0x30, 0xC5),
            (&[0xFB, 0x30, 0x00, 0x00][..], 0x31, 0x0F),
            (&[0xFB, 0x40, 0x00, 0x00][..], 0x30, 0xD4),
            (&[0xFB, 0x35, 0x00, 0x01, 0x80][..], 0xCE, 0x28),
            (&[0xFA, 0x35, 0x00, 0x01, 0x01][..], 0x33, 0x88),
            (&[0xFA, 0x30, 0x00, 0x01, 0x00][..], 0xF2, 0x84),
        ] {
            assert_eq!(crc16_modbus(frame).to_le_bytes(), [lo, hi], "{frame:02X?}");
        }
    }
}
