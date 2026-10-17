//! Frame layout: `addr | func | seq | len | payload[len] | crc16 LE`, padded to 64 bytes.

use crate::{ProtoError, crc16_modbus};

/// Size of an HID input/output report (without the report-ID byte).
pub const REPORT_LEN: usize = 64;
/// Maximum payload: 64 − 4 header bytes − 2 CRC bytes.
pub const MAX_PAYLOAD: usize = REPORT_LEN - 6;
/// Address byte of host → device frames.
pub const HOST_ADDR: u8 = 0xFB;
/// Address byte of device → host frames.
pub const DEVICE_ADDR: u8 = 0xFA;

/// Function codes this crate speaks. Firmware-upgrade codes (0x11–0x15),
/// the unused 0x45 and the firmware-driven scan/sequence codes (0x50/0x55)
/// are intentionally absent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Func {
    DeviceInfo = 0x10,
    BasicInfo = 0x30,
    BasicSet = 0x35,
    SystemInfo = 0x40,
}

impl TryFrom<u8> for Func {
    type Error = ProtoError;
    fn try_from(v: u8) -> Result<Self, ProtoError> {
        Ok(match v {
            0x10 => Func::DeviceInfo,
            0x30 => Func::BasicInfo,
            0x35 => Func::BasicSet,
            0x40 => Func::SystemInfo,
            other => return Err(ProtoError::UnknownFunc(other)),
        })
    }
}

/// A decoded or to-be-encoded frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub func: Func,
    /// Sequence number; the device echoes it back unchanged.
    pub seq: u8,
    payload: [u8; MAX_PAYLOAD],
    len: u8,
}

impl Frame {
    /// Build a frame. Fails if the payload exceeds [`MAX_PAYLOAD`].
    pub fn new(func: Func, seq: u8, payload: &[u8]) -> Result<Self, ProtoError> {
        if payload.len() > MAX_PAYLOAD {
            return Err(ProtoError::PayloadTooLong(payload.len()));
        }
        let mut buf = [0u8; MAX_PAYLOAD];
        buf[..payload.len()].copy_from_slice(payload);
        Ok(Self { func, seq, payload: buf, len: payload.len() as u8 })
    }

    /// The payload bytes (exactly `len` of them).
    pub fn payload(&self) -> &[u8] {
        &self.payload[..usize::from(self.len)]
    }

    /// Encode as a host → device output report (64 bytes, zero padded).
    ///
    /// The HID transport must prefix it with report ID `0x00`.
    pub fn encode(&self) -> [u8; REPORT_LEN] {
        self.encode_with_addr(HOST_ADDR)
    }

    fn encode_with_addr(&self, addr: u8) -> [u8; REPORT_LEN] {
        let n = usize::from(self.len);
        let mut out = [0u8; REPORT_LEN];
        out[0] = addr;
        out[1] = self.func as u8;
        out[2] = self.seq;
        out[3] = self.len;
        out[4..4 + n].copy_from_slice(self.payload());
        let crc = crc16_modbus(&out[..4 + n]);
        out[4 + n..6 + n].copy_from_slice(&crc.to_le_bytes());
        out
    }

    /// Encoded length on the wire, without padding.
    pub fn wire_len(&self) -> usize {
        6 + usize::from(self.len)
    }

    /// Decode a device → host input report. Bytes after the CRC are ignored:
    /// the firmware leaves stale data from earlier replies there.
    pub fn decode(report: &[u8]) -> Result<Self, ProtoError> {
        Self::decode_with_addr(report, DEVICE_ADDR)
    }

    /// Decode a host → device report. Only meant for simulators and tests.
    pub fn decode_from_host(report: &[u8]) -> Result<Self, ProtoError> {
        Self::decode_with_addr(report, HOST_ADDR)
    }

    fn decode_with_addr(report: &[u8], addr: u8) -> Result<Self, ProtoError> {
        if report.len() < 6 {
            return Err(ProtoError::TooShort(report.len()));
        }
        if report[0] != addr {
            return Err(ProtoError::BadAddress(report[0]));
        }
        let n = usize::from(report[3]);
        if n > MAX_PAYLOAD || 6 + n > report.len() {
            return Err(ProtoError::BadLength(n));
        }
        let computed = crc16_modbus(&report[..4 + n]);
        let received = u16::from_le_bytes([report[4 + n], report[5 + n]]);
        if computed != received {
            return Err(ProtoError::Crc { computed, received });
        }
        let func = Func::try_from(report[1])?;
        Self::new(func, report[2], &report[4..4 + n])
    }

    /// Encode as a device → host report. Only meant for tests and simulators.
    pub fn encode_as_device(&self) -> [u8; REPORT_LEN] {
        self.encode_with_addr(DEVICE_ADDR)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        s.split_whitespace().map(|b| u8::from_str_radix(b, 16).unwrap()).collect()
    }

    #[test]
    fn encode_requests_match_wire() {
        let f = Frame::new(Func::DeviceInfo, 0, &[]).unwrap();
        assert_eq!(&f.encode()[..f.wire_len()], &hex("FB 10 00 00 30 C5")[..]);
        let f = Frame::new(Func::BasicSet, 0, &[0x80]).unwrap();
        assert_eq!(&f.encode()[..f.wire_len()], &hex("FB 35 00 01 80 CE 28")[..]);
        let f = Frame::new(Func::BasicInfo, 0x5A, &[]).unwrap();
        assert_eq!(&f.encode()[..f.wire_len()], &hex("FB 30 5A 00 0B AF")[..]);
        assert!(f.encode()[6..].iter().all(|&b| b == 0));
    }

    #[test]
    fn decode_ignores_stale_tail() {
        // Real HW reply (2026-10-17) with the stale tail the firmware leaves.
        let mut r = hex("FA 30 00 10 77 3B 84 13 00 00 C6 39 1C 01 1A 01 DB 13 01 00 36 26 \
                         0F 00 0B 00 AA 00 96 E2 99 0E 00 40 75 30 03 A7 4D 05 EA 07 09 1C 92 67");
        r.resize(64, 0);
        let f = Frame::decode(&r).unwrap();
        assert_eq!(f.func, Func::BasicInfo);
        assert_eq!(f.payload().len(), 16);
    }

    #[test]
    fn decode_seq_echo() {
        let mut r = hex("FA 30 5A 10 7A 3B 85 13 00 00 C6 39 1B 01 19 01 DB 13 01 00 81 6C");
        r.resize(64, 0);
        assert_eq!(Frame::decode(&r).unwrap().seq, 0x5A);
    }

    #[test]
    fn decode_errors() {
        let mut r = hex("FA 30 00 01 00 F2 85");
        r.resize(64, 0);
        assert!(matches!(Frame::decode(&r), Err(ProtoError::Crc { .. })));
        r[0] = 0xFB;
        assert_eq!(Frame::decode(&r), Err(ProtoError::BadAddress(0xFB)));
        assert_eq!(Frame::decode(&[0xFA, 0x30]), Err(ProtoError::TooShort(2)));
        let mut r = vec![0xFA, 0x30, 0, 59];
        r.resize(64, 0);
        assert_eq!(Frame::decode(&r), Err(ProtoError::BadLength(59)));
        let mut r = hex("FA 15 00 00");
        let crc = crc16_modbus(&r);
        r.extend_from_slice(&crc.to_le_bytes());
        assert_eq!(Frame::decode(&r), Err(ProtoError::UnknownFunc(0x15)));
    }

    #[test]
    fn roundtrip_device_side() {
        let f = Frame::new(Func::SystemInfo, 7, &[1, 2, 3]).unwrap();
        assert_eq!(Frame::decode(&f.encode_as_device()).unwrap(), f);
        assert!(Frame::new(Func::SystemInfo, 0, &[0; 59]).is_err());
    }
}
