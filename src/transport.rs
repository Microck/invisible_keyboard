//! Bounded COBS framing and direction-aware USB CDC record dispatch.

use std::{error::Error, fmt};

use crate::protocol::{
    FRAME_LEN, FrameError, MAGIC, QUIESCE_RECEIPT_LEN, QUIESCE_RECEIPT_MAGIC, QUIESCE_REQUEST_LEN,
    QUIESCE_REQUEST_MAGIC, QuiesceReceipt, QuiesceRecordError, QuiesceRequest, SensorFrame,
    decode_frame, decode_quiesce_receipt, encode_frame, encode_quiesce_receipt,
    encode_quiesce_request,
};

/// Largest COBS payload produced by one canonical raw frame.
pub const MAX_ENCODED_FRAME_LEN: usize = FRAME_LEN + (FRAME_LEN / 254) + 1;

/// Largest complete transport record, including its zero delimiter.
pub const MAX_TRANSPORT_RECORD_LEN: usize = MAX_ENCODED_FRAME_LEN + 1;

/// Exact COBS payload length for an `IKQ2` request, excluding its delimiter.
pub const QUIESCE_REQUEST_ENCODED_LEN: usize = QUIESCE_REQUEST_LEN + 1;

/// Exact request length on the byte stream, including its zero delimiter.
pub const QUIESCE_REQUEST_TRANSPORT_LEN: usize = QUIESCE_REQUEST_ENCODED_LEN + 1;

/// Exact COBS payload length for an `IKR2` receipt, excluding its delimiter.
pub const QUIESCE_RECEIPT_ENCODED_LEN: usize = QUIESCE_RECEIPT_LEN + 1;

/// Exact receipt length on the byte stream, including its zero delimiter.
pub const QUIESCE_RECEIPT_TRANSPORT_LEN: usize = QUIESCE_RECEIPT_ENCODED_LEN + 1;

/// One semantically valid record received from a controller.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ControllerRecord {
    SensorFrame(SensorFrame),
    QuiesceReceipt(QuiesceReceipt),
}

/// Why decoded bytes cannot be dispatched as a controller-to-host record.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ControllerRecordError {
    UnknownMagic,
    /// `IKQ2` is valid only in the host-to-controller direction.
    WrongDirection,
    Frame(FrameError),
    Quiesce(QuiesceRecordError),
}

impl fmt::Display for ControllerRecordError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownMagic => formatter.write_str("controller record has unknown magic"),
            Self::WrongDirection => {
                formatter.write_str("controller emitted a host-to-controller IKQ2 request")
            }
            Self::Frame(error) => error.fmt(formatter),
            Self::Quiesce(error) => error.fmt(formatter),
        }
    }
}

impl Error for ControllerRecordError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Frame(error) => Some(error),
            Self::Quiesce(error) => Some(error),
            Self::UnknownMagic | Self::WrongDirection => None,
        }
    }
}

/// One candidate recovered at a zero delimiter or at disconnect.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TransportCandidate {
    /// COBS decoded successfully. Raw-frame validation remains a separate step.
    Decoded(Vec<u8>),
    /// COBS structure was invalid. The encoded candidate is retained.
    Malformed { encoded: Vec<u8> },
    /// A candidate exceeded the one-frame bound and was discarded to delimiter.
    Oversized { prefix: Vec<u8>, actual_len: usize },
    /// Disconnect occurred before a trailing delimiter.
    Unterminated { encoded: Vec<u8> },
}

/// Stateful, chunk-boundary-independent COBS stream decoder.
#[derive(Clone, Debug)]
pub struct TransportDecoder {
    encoded: [u8; MAX_ENCODED_FRAME_LEN],
    encoded_len: usize,
    actual_len: usize,
}

impl Default for TransportDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl TransportDecoder {
    /// Create an empty decoder.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            encoded: [0; MAX_ENCODED_FRAME_LEN],
            encoded_len: 0,
            actual_len: 0,
        }
    }

    /// Consume an arbitrary stream chunk and return every completed candidate.
    #[must_use]
    pub fn push(&mut self, bytes: &[u8]) -> Vec<TransportCandidate> {
        let mut completed = Vec::new();
        for byte in bytes {
            if *byte == 0 {
                if let Some(candidate) = self.complete_delimited() {
                    completed.push(candidate);
                }
                continue;
            }

            self.actual_len = self.actual_len.saturating_add(1);
            if self.encoded_len < MAX_ENCODED_FRAME_LEN {
                self.encoded[self.encoded_len] = *byte;
                self.encoded_len += 1;
            }
        }
        completed
    }

    /// Drain a partial candidate when EOF or disconnect ends the stream.
    #[must_use]
    pub fn finish(&mut self) -> Option<TransportCandidate> {
        if self.actual_len == 0 {
            return None;
        }
        if self.actual_len > MAX_ENCODED_FRAME_LEN {
            return Some(self.take_oversized());
        }
        self.actual_len = 0;
        let encoded = self.encoded[..self.encoded_len].to_vec();
        self.encoded_len = 0;
        Some(TransportCandidate::Unterminated { encoded })
    }

    fn complete_delimited(&mut self) -> Option<TransportCandidate> {
        if self.actual_len == 0 {
            return None;
        }
        if self.actual_len > MAX_ENCODED_FRAME_LEN {
            return Some(self.take_oversized());
        }

        self.actual_len = 0;
        let encoded = &self.encoded[..self.encoded_len];
        let candidate = match cobs_decode(encoded) {
            Some(decoded) => Some(TransportCandidate::Decoded(decoded)),
            None => Some(TransportCandidate::Malformed {
                encoded: encoded.to_vec(),
            }),
        };
        self.encoded_len = 0;
        candidate
    }

    fn take_oversized(&mut self) -> TransportCandidate {
        let actual_len = std::mem::take(&mut self.actual_len);
        let prefix = self.encoded[..self.encoded_len].to_vec();
        self.encoded_len = 0;
        TransportCandidate::Oversized { prefix, actual_len }
    }
}

/// Encode one canonical frame as a COBS payload plus zero delimiter.
pub fn encode_transport_frame(frame: &SensorFrame) -> Result<Vec<u8>, FrameError> {
    let raw = encode_frame(frame)?;
    let encoded = encode_transport_record(&raw);
    debug_assert!(encoded.len() <= MAX_TRANSPORT_RECORD_LEN);
    Ok(encoded)
}

/// Encode one exact `IKQ2` request as COBS plus its zero delimiter.
pub fn encode_transport_quiesce_request(
    request: &QuiesceRequest,
) -> Result<Vec<u8>, QuiesceRecordError> {
    let encoded = encode_transport_record(&encode_quiesce_request(request)?);
    debug_assert_eq!(encoded.len(), QUIESCE_REQUEST_TRANSPORT_LEN);
    Ok(encoded)
}

/// Encode one exact `IKR2` receipt as COBS plus its zero delimiter.
pub fn encode_transport_quiesce_receipt(
    receipt: &QuiesceReceipt,
) -> Result<Vec<u8>, QuiesceRecordError> {
    let encoded = encode_transport_record(&encode_quiesce_receipt(receipt)?);
    debug_assert_eq!(encoded.len(), QUIESCE_RECEIPT_TRANSPORT_LEN);
    Ok(encoded)
}

/// Decode one COBS-decoded controller-to-host record with direction checks.
///
/// This dispatch runs after [`TransportDecoder`] has recovered a
/// [`TransportCandidate::Decoded`] payload. It never interprets outbound
/// `IKQ2` bytes as controller evidence.
pub fn decode_controller_record(bytes: &[u8]) -> Result<ControllerRecord, ControllerRecordError> {
    match bytes.get(..4) {
        Some(magic) if magic == MAGIC => decode_frame(bytes)
            .map(ControllerRecord::SensorFrame)
            .map_err(ControllerRecordError::Frame),
        Some(magic) if magic == QUIESCE_RECEIPT_MAGIC => decode_quiesce_receipt(bytes)
            .map(ControllerRecord::QuiesceReceipt)
            .map_err(ControllerRecordError::Quiesce),
        Some(magic) if magic == QUIESCE_REQUEST_MAGIC => Err(ControllerRecordError::WrongDirection),
        _ => Err(ControllerRecordError::UnknownMagic),
    }
}

fn encode_transport_record(raw: &[u8]) -> Vec<u8> {
    let mut encoded = cobs_encode(raw);
    encoded.push(0);
    encoded
}

fn cobs_encode(input: &[u8]) -> Vec<u8> {
    let mut output = Vec::with_capacity(input.len() + (input.len() / 254) + 1);
    output.push(0);
    let mut code_index = 0;
    let mut code = 1_u8;

    for byte in input {
        if *byte == 0 {
            output[code_index] = code;
            code_index = output.len();
            output.push(0);
            code = 1;
            continue;
        }

        output.push(*byte);
        code = code.saturating_add(1);
        if code == u8::MAX {
            output[code_index] = code;
            code_index = output.len();
            output.push(0);
            code = 1;
        }
    }

    output[code_index] = code;
    output
}

fn cobs_decode(input: &[u8]) -> Option<Vec<u8>> {
    let mut output = Vec::with_capacity(input.len().saturating_sub(1));
    let mut index = 0;

    while index < input.len() {
        let code = usize::from(input[index]);
        if code == 0 {
            return None;
        }
        index += 1;

        let run_length = code - 1;
        let run_end = index.checked_add(run_length)?;
        if run_end > input.len() {
            return None;
        }
        output.extend_from_slice(&input[index..run_end]);
        index = run_end;

        if code != usize::from(u8::MAX) && index < input.len() {
            output.push(0);
        }
    }

    Some(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cobs_round_trips_zero_patterns_and_long_runs() {
        let cases = [
            Vec::new(),
            vec![0],
            vec![0, 0, 0],
            vec![1, 2, 3],
            vec![1, 0, 2, 0, 3],
            (1_u8..=u8::MAX).collect(),
            vec![7; 700],
        ];

        for input in cases {
            let encoded = cobs_encode(&input);
            assert!(!encoded.contains(&0));
            assert_eq!(cobs_decode(&encoded), Some(input));
        }
    }

    #[test]
    fn malformed_cobs_run_is_rejected() {
        assert_eq!(cobs_decode(&[4, 1, 2]), None);
        assert_eq!(cobs_decode(&[0]), None);
    }
}
