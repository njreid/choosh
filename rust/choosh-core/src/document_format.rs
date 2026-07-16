//! Bounded text-format classification and lossless document codec.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FormatLimits {
    pub max_encoded_bytes: usize,
    pub max_decoded_bytes: usize,
    pub max_lines: usize,
    pub binary_probe_bytes: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Encoding {
    Utf8,
    Utf16Le,
    Utf16Be,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BomPolicy {
    None,
    Present,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LineEnding {
    Lf,
    CrLf,
    None,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TextFormat {
    pub encoding: Encoding,
    pub bom: BomPolicy,
    pub line_ending: LineEnding,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReadOnlyReason {
    Binary,
    UnsupportedEncoding,
    MixedLineEndings,
    Oversized,
    InvalidText,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Classification {
    Writable {
        text: String,
        format: TextFormat,
        lines: usize,
    },
    ReadOnly {
        reason: ReadOnlyReason,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FormatError {
    InvalidLimits,
    EncodedTooLarge,
    DecodedTooLarge,
    TooManyLines,
    InvalidText,
    Unrepresentable,
    LineEndingMismatch,
}

/// Classifies and decodes complete encoded bytes within explicit bounds.
///
/// # Errors
///
/// Returns [`FormatError::InvalidLimits`] for zero limits. Content faults are
/// classified read-only so callers can present metadata without editing.
pub fn classify(bytes: &[u8], limits: FormatLimits) -> Result<Classification, FormatError> {
    validate_limits(limits)?;
    if bytes.len() > limits.max_encoded_bytes {
        return Ok(Classification::ReadOnly {
            reason: ReadOnlyReason::Oversized,
        });
    }
    let Some((encoding, bom)) = detect_encoding(bytes) else {
        return Ok(Classification::ReadOnly {
            reason: ReadOnlyReason::UnsupportedEncoding,
        });
    };
    let payload = match (encoding, bom) {
        (Encoding::Utf8, BomPolicy::Present) => &bytes[3..],
        (Encoding::Utf16Le | Encoding::Utf16Be, BomPolicy::Present) => &bytes[2..],
        (_, BomPolicy::None) => bytes,
    };
    if contains_binary_nul(payload, encoding, limits.binary_probe_bytes) {
        return Ok(Classification::ReadOnly {
            reason: ReadOnlyReason::Binary,
        });
    }
    let text = match decode(payload, encoding, limits.max_decoded_bytes) {
        Ok(text) => text,
        Err(FormatError::DecodedTooLarge) => {
            return Ok(Classification::ReadOnly {
                reason: ReadOnlyReason::Oversized,
            });
        }
        Err(FormatError::InvalidText) => {
            return Ok(Classification::ReadOnly {
                reason: ReadOnlyReason::InvalidText,
            });
        }
        Err(error) => return Err(error),
    };
    let Some(line_ending) = classify_line_endings(&text) else {
        return Ok(Classification::ReadOnly {
            reason: ReadOnlyReason::MixedLineEndings,
        });
    };
    let lines = count_lines(&text)?;
    if lines > limits.max_lines {
        return Ok(Classification::ReadOnly {
            reason: ReadOnlyReason::Oversized,
        });
    }
    Ok(Classification::Writable {
        text,
        format: TextFormat {
            encoding,
            bom,
            line_ending,
        },
        lines,
    })
}

/// Encodes canonical text while preserving its classified BOM and line endings.
///
/// # Errors
///
/// Rejects mixed/wrong line endings, encoded/decoded limits, line overflow, and
/// text that cannot be represented by the selected encoding.
pub fn encode(
    text: &str,
    format: TextFormat,
    limits: FormatLimits,
) -> Result<Vec<u8>, FormatError> {
    validate_limits(limits)?;
    if text.len() > limits.max_decoded_bytes {
        return Err(FormatError::DecodedTooLarge);
    }
    if count_lines(text)? > limits.max_lines {
        return Err(FormatError::TooManyLines);
    }
    if classify_line_endings(text) != Some(format.line_ending) {
        return Err(FormatError::LineEndingMismatch);
    }
    let mut result = Vec::new();
    if format.bom == BomPolicy::Present {
        result.extend_from_slice(match format.encoding {
            Encoding::Utf8 => b"\xef\xbb\xbf",
            Encoding::Utf16Le => b"\xff\xfe",
            Encoding::Utf16Be => b"\xfe\xff",
        });
    }
    match format.encoding {
        Encoding::Utf8 => result.extend_from_slice(text.as_bytes()),
        Encoding::Utf16Le => {
            for unit in text.encode_utf16() {
                result.extend_from_slice(&unit.to_le_bytes());
            }
        }
        Encoding::Utf16Be => {
            for unit in text.encode_utf16() {
                result.extend_from_slice(&unit.to_be_bytes());
            }
        }
    }
    if result.len() > limits.max_encoded_bytes {
        Err(FormatError::EncodedTooLarge)
    } else {
        Ok(result)
    }
}

fn validate_limits(limits: FormatLimits) -> Result<(), FormatError> {
    if limits.max_encoded_bytes == 0
        || limits.max_decoded_bytes == 0
        || limits.max_lines == 0
        || limits.binary_probe_bytes == 0
    {
        Err(FormatError::InvalidLimits)
    } else {
        Ok(())
    }
}

fn detect_encoding(bytes: &[u8]) -> Option<(Encoding, BomPolicy)> {
    if bytes.starts_with(b"\xef\xbb\xbf") {
        Some((Encoding::Utf8, BomPolicy::Present))
    } else if bytes.starts_with(b"\xff\xfe") {
        Some((Encoding::Utf16Le, BomPolicy::Present))
    } else if bytes.starts_with(b"\xfe\xff") {
        Some((Encoding::Utf16Be, BomPolicy::Present))
    } else if std::str::from_utf8(bytes).is_ok() {
        Some((Encoding::Utf8, BomPolicy::None))
    } else {
        None
    }
}

fn contains_binary_nul(bytes: &[u8], encoding: Encoding, probe: usize) -> bool {
    let end = bytes.len().min(probe);
    match encoding {
        Encoding::Utf8 => bytes[..end].contains(&0),
        Encoding::Utf16Le | Encoding::Utf16Be => false,
    }
}

fn decode(bytes: &[u8], encoding: Encoding, max_decoded: usize) -> Result<String, FormatError> {
    match encoding {
        Encoding::Utf8 => {
            if bytes.len() > max_decoded {
                return Err(FormatError::DecodedTooLarge);
            }
            std::str::from_utf8(bytes)
                .map(str::to_owned)
                .map_err(|_| FormatError::InvalidText)
        }
        Encoding::Utf16Le | Encoding::Utf16Be => {
            if !bytes.len().is_multiple_of(2) {
                return Err(FormatError::InvalidText);
            }
            let units = bytes.chunks_exact(2).map(|pair| match encoding {
                Encoding::Utf16Le => u16::from_le_bytes([pair[0], pair[1]]),
                Encoding::Utf16Be => u16::from_be_bytes([pair[0], pair[1]]),
                Encoding::Utf8 => unreachable!(),
            });
            let mut text = String::new();
            for value in char::decode_utf16(units) {
                let character = value.map_err(|_| FormatError::InvalidText)?;
                if text
                    .len()
                    .checked_add(character.len_utf8())
                    .ok_or(FormatError::DecodedTooLarge)?
                    > max_decoded
                {
                    return Err(FormatError::DecodedTooLarge);
                }
                text.push(character);
            }
            Ok(text)
        }
    }
}

fn classify_line_endings(text: &str) -> Option<LineEnding> {
    let bytes = text.as_bytes();
    let mut lf = false;
    let mut crlf = false;
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'\r' if bytes.get(index + 1) == Some(&b'\n') => {
                crlf = true;
                index += 2;
            }
            b'\r' => return None,
            b'\n' => {
                lf = true;
                index += 1;
            }
            _ => index += 1,
        }
    }
    match (lf, crlf) {
        (true, true) => None,
        (true, false) => Some(LineEnding::Lf),
        (false, true) => Some(LineEnding::CrLf),
        (false, false) => Some(LineEnding::None),
    }
}

fn count_lines(text: &str) -> Result<usize, FormatError> {
    text.bytes()
        .filter(|byte| *byte == b'\n')
        .count()
        .checked_add(1)
        .ok_or(FormatError::TooManyLines)
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIMITS: FormatLimits = FormatLimits {
        max_encoded_bytes: 64,
        max_decoded_bytes: 48,
        max_lines: 4,
        binary_probe_bytes: 16,
    };

    #[test]
    fn utf8_bom_and_line_endings_round_trip_exactly() {
        for bytes in [b"plain\ntext".as_slice(), b"\xef\xbb\xbfcrlf\r\ntext"] {
            let Classification::Writable { text, format, .. } = classify(bytes, LIMITS).unwrap()
            else {
                panic!("expected writable")
            };
            assert_eq!(encode(&text, format, LIMITS).unwrap(), bytes);
        }
    }

    #[test]
    fn utf16_endianness_surrogates_and_bom_round_trip() {
        let text = "hello 🙂\r\n";
        for format in [
            TextFormat {
                encoding: Encoding::Utf16Le,
                bom: BomPolicy::Present,
                line_ending: LineEnding::CrLf,
            },
            TextFormat {
                encoding: Encoding::Utf16Be,
                bom: BomPolicy::Present,
                line_ending: LineEnding::CrLf,
            },
        ] {
            let bytes = encode(text, format, LIMITS).unwrap();
            assert_eq!(
                classify(&bytes, LIMITS).unwrap(),
                Classification::Writable {
                    text: text.into(),
                    format,
                    lines: 2
                }
            );
        }
    }

    #[test]
    fn binary_mixed_invalid_and_unsupported_are_read_only() {
        assert_eq!(
            classify(b"a\0b", LIMITS).unwrap(),
            Classification::ReadOnly {
                reason: ReadOnlyReason::Binary
            }
        );
        assert_eq!(
            classify(b"a\nb\r\n", LIMITS).unwrap(),
            Classification::ReadOnly {
                reason: ReadOnlyReason::MixedLineEndings
            }
        );
        assert_eq!(
            classify(b"\xffx", LIMITS).unwrap(),
            Classification::ReadOnly {
                reason: ReadOnlyReason::UnsupportedEncoding
            }
        );
        assert_eq!(
            classify(b"\xff\xfe\x00\xd8", LIMITS).unwrap(),
            Classification::ReadOnly {
                reason: ReadOnlyReason::InvalidText
            }
        );
    }

    #[test]
    fn byte_decoded_and_line_limits_classify_or_reject_deterministically() {
        let small = FormatLimits {
            max_encoded_bytes: 2,
            ..LIMITS
        };
        assert_eq!(
            classify(b"abc", small).unwrap(),
            Classification::ReadOnly {
                reason: ReadOnlyReason::Oversized
            }
        );
        let lines = FormatLimits {
            max_lines: 2,
            ..LIMITS
        };
        assert_eq!(
            classify(b"a\nb\nc", lines).unwrap(),
            Classification::ReadOnly {
                reason: ReadOnlyReason::Oversized
            }
        );
        assert_eq!(
            encode(
                "a\nb\nc",
                TextFormat {
                    encoding: Encoding::Utf8,
                    bom: BomPolicy::None,
                    line_ending: LineEnding::Lf
                },
                lines
            ),
            Err(FormatError::TooManyLines)
        );
    }

    #[test]
    fn encoding_rejects_line_policy_mismatch_and_encoded_overflow() {
        let format = TextFormat {
            encoding: Encoding::Utf8,
            bom: BomPolicy::Present,
            line_ending: LineEnding::CrLf,
        };
        assert_eq!(
            encode("a\n", format, LIMITS),
            Err(FormatError::LineEndingMismatch)
        );
        let tiny = FormatLimits {
            max_encoded_bytes: 3,
            ..LIMITS
        };
        assert_eq!(
            encode(
                "a",
                TextFormat {
                    encoding: Encoding::Utf8,
                    bom: BomPolicy::Present,
                    line_ending: LineEnding::None
                },
                tiny
            ),
            Err(FormatError::EncodedTooLarge)
        );
    }
}
