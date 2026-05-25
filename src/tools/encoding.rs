use encoding_rs::{GBK, BIG5, SHIFT_JIS, EUC_JP, EUC_KR, WINDOWS_1252, ISO_8859_2};
use serde::{Deserialize, Serialize};

/// DetectedEncoding represents the result of encoding detection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DetectedEncoding {
    pub encoding: String,
    pub confidence: f64,
}

/// Detect the encoding of raw bytes.
pub fn detect_encoding(data: &[u8]) -> DetectedEncoding {
    // Try UTF-8 first
    if let Ok(_) = std::str::from_utf8(data) {
        return DetectedEncoding {
            encoding: "utf-8".to_string(),
            confidence: 1.0,
        };
    }

    // Try BOM detection
    if data.len() >= 3 && data[0] == 0xEF && data[1] == 0xBB && data[2] == 0xBF {
        return DetectedEncoding {
            encoding: "utf-8-bom".to_string(),
            confidence: 1.0,
        };
    }
    if data.len() >= 2 && data[0] == 0xFF && data[1] == 0xFE {
        return DetectedEncoding {
            encoding: "utf-16-le".to_string(),
            confidence: 1.0,
        };
    }
    if data.len() >= 2 && data[0] == 0xFE && data[1] == 0xFF {
        return DetectedEncoding {
            encoding: "utf-16-be".to_string(),
            confidence: 1.0,
        };
    }

    // Try common encodings with heuristic
    let encodings = vec![
        ("gbk", GBK),
        ("big5", BIG5),
        ("shift_jis", SHIFT_JIS),
        ("euc-jp", EUC_JP),
        ("euc-kr", EUC_KR),
        ("iso-8859-1", WINDOWS_1252),
        ("windows-1252", WINDOWS_1252),
    ];

    for (name, encoding) in &encodings {
        let (decoded, _, had_errors) = encoding.decode(data);
        if !had_errors && decoded.len() > 0 {
            // Simple heuristic: if decoding succeeds and produces reasonable text
            let printable_ratio = decoded.chars().filter(|c| !c.is_control()).count() as f64
                / decoded.len().max(1) as f64;
            if printable_ratio > 0.9 {
                return DetectedEncoding {
                    encoding: name.to_string(),
                    confidence: printable_ratio,
                };
            }
        }
    }

    DetectedEncoding {
        encoding: "utf-8".to_string(), // fallback
        confidence: 0.0,
    }
}

/// Convert bytes from one encoding to UTF-8.
pub fn convert_to_utf8(data: &[u8], from_encoding: &str) -> Result<String, String> {
    let encoding = match from_encoding.to_lowercase().as_str() {
        "utf-8" | "utf8" => return Ok(String::from_utf8_lossy(data).into_owned()),
        "utf-8-bom" => {
            let start = if data.len() >= 3 && data[0] == 0xEF && data[1] == 0xBB && data[2] == 0xBF {
                3
            } else {
                0
            };
            return Ok(String::from_utf8_lossy(&data[start..]).into_owned());
        }
        "gbk" | "gb2312" | "gb18030" => GBK,
        "big5" | "big-5" => BIG5,
        "shift_jis" | "shift-jis" | "sjis" => SHIFT_JIS,
        "euc-jp" => EUC_JP,
        "euc-kr" => EUC_KR,
        "iso-8859-1" | "latin1" => WINDOWS_1252,
        "windows-1252" | "cp1252" => WINDOWS_1252,
        _ => return Err(format!("Unsupported encoding: {}", from_encoding)),
    };

    let (decoded, _, had_errors) = encoding.decode(data);
    if had_errors {
        Err(format!("Failed to decode from {}", from_encoding))
    } else {
        Ok(decoded.into_owned())
    }
}

/// Detect and convert bytes to UTF-8 string.
pub fn decode_bytes(data: &[u8]) -> String {
    let detected = detect_encoding(data);
    match convert_to_utf8(data, &detected.encoding) {
        Ok(s) => s,
        Err(_) => String::from_utf8_lossy(data).into_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_utf8() {
        let data = b"hello world";
        let result = detect_encoding(data);
        assert_eq!(result.encoding, "utf-8");
        assert_eq!(result.confidence, 1.0);
    }

    #[test]
    fn test_detect_utf8_bom() {
        let data = [0xEF, 0xBB, 0xBF, b'h', b'e', b'l', b'l', b'o'];
        let result = detect_encoding(&data);
        assert_eq!(result.encoding, "utf-8-bom");
    }

    #[test]
    fn test_detect_utf16_le() {
        let data = [0xFF, 0xFE, b'h', 0x00, b'i', 0x00];
        let result = detect_encoding(&data);
        assert_eq!(result.encoding, "utf-16-le");
    }

    #[test]
    fn test_convert_utf8() {
        let data = b"hello";
        let result = convert_to_utf8(data, "utf-8").unwrap();
        assert_eq!(result, "hello");
    }

    #[test]
    fn test_convert_utf8_bom() {
        let data = [0xEF, 0xBB, 0xBF, b'h', b'e', b'l', b'l', b'o'];
        let result = convert_to_utf8(&data, "utf-8-bom").unwrap();
        assert_eq!(result, "hello");
    }

    #[test]
    fn test_convert_latin1() {
        let data = [0x48, 0x65, 0x6C, 0x6C, 0x6F]; // "Hello"
        let result = convert_to_utf8(&data, "iso-8859-1").unwrap();
        assert_eq!(result, "Hello");
    }

    #[test]
    fn test_decode_bytes_utf8() {
        let data = b"hello world";
        let result = decode_bytes(data);
        assert_eq!(result, "hello world");
    }

    #[test]
    fn test_unsupported_encoding() {
        let data = b"test";
        let result = convert_to_utf8(data, "unknown-encoding");
        assert!(result.is_err());
    }
}
