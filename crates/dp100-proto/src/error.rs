use crate::frame::Func;

/// Errors raised while encoding or decoding DP100 frames.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProtoError {
    #[error("report too short: {0} bytes")]
    TooShort(usize),
    #[error("unexpected address byte 0x{0:02X}")]
    BadAddress(u8),
    #[error("unknown or unsupported function code 0x{0:02X}")]
    UnknownFunc(u8),
    #[error("declared payload length {0} exceeds the report")]
    BadLength(usize),
    #[error("CRC mismatch: computed 0x{computed:04X}, received 0x{received:04X}")]
    Crc { computed: u16, received: u16 },
    #[error("payload too long: {0} bytes (max 58)")]
    PayloadTooLong(usize),
    #[error("unexpected payload length {got} for {func:?} (expected {expected})")]
    PayloadLen { func: Func, expected: &'static str, got: usize },
    #[error("device rejected the request (NAK) for {0:?}")]
    Nak(Func),
    #[error("response function {got:?} does not match request {expected:?}")]
    FuncMismatch { expected: Func, got: Func },
    #[error("invalid preset index {0} (valid: 0-9)")]
    BadPreset(u8),
    #[error("preset 0 cannot be saved or recalled (the vendor DLL refuses it too)")]
    PresetZeroReadOnly,
}
