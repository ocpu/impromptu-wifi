use core::fmt;

/// Why a byte sequence could not be decoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum DecodeError {
    /// More bytes are needed before the frame is complete.
    Incomplete,
    /// A length field disagrees with the number of bytes present.
    LengthMismatch,
    /// The checksum byte does not match the frame contents.
    Checksum,
    /// A string field is not valid UTF-8.
    BadUtf8,
    /// A field exceeds a limit set by the spec (for example a 33-byte SSID).
    FieldTooLong,
    /// A serial packet did not start with `IMPROV`.
    BadHeader,
    /// A serial packet carried an unsupported protocol version.
    BadVersion(u8),
    /// A serial packet carried an unknown packet type.
    UnknownPacketType(u8),
    /// The frame is larger than the buffer it is being assembled in.
    TooLarge,
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Incomplete => f.write_str("incomplete frame"),
            Self::LengthMismatch => f.write_str("length field does not match frame"),
            Self::Checksum => f.write_str("checksum mismatch"),
            Self::BadUtf8 => f.write_str("string is not valid UTF-8"),
            Self::FieldTooLong => f.write_str("field exceeds protocol limit"),
            Self::BadHeader => f.write_str("missing IMPROV header"),
            Self::BadVersion(v) => write!(f, "unsupported protocol version {v}"),
            Self::UnknownPacketType(t) => write!(f, "unknown packet type {t:#04x}"),
            Self::TooLarge => f.write_str("frame larger than buffer"),
        }
    }
}

impl core::error::Error for DecodeError {}

/// Why a value could not be encoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum EncodeError {
    /// The output doesn't fit in the buffer, or exceeds the protocol's
    /// one-byte length limits.
    Overflow,
    /// The command doesn't exist on this transport.
    WrongTransport,
}

impl fmt::Display for EncodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Overflow => f.write_str("output does not fit buffer or protocol length limit"),
            Self::WrongTransport => f.write_str("command does not exist on this transport"),
        }
    }
}

impl core::error::Error for EncodeError {}
