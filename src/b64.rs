//! Base64 编解码（RFC 4648 标准字母表 + padding），手写实现零依赖。
//!
//! 用途：DPAPI 密文是任意字节，落 INI 配置文件必须转成可打印文本，
//! 故密文以 base64 编码存于 `enc:v1:dpapi:` 前缀之后（见 [`crate::config`]）。
//!
//! 严格解码：只接受标准字母表字符，`=` 只允许出现在末尾且至多两个，
//! 长度必须是 4 的倍数——拒绝宽松解析，坏密文一律 fail-closed。

/// RFC 4648 标准字母表（A-Z a-z 0-9 + /），padding 为 `=`。
const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// 编码为带 padding 的标准 base64（空输入返回空串）。
pub fn encode(data: &[u8]) -> String {
    let mut out = String::with_capacity((data.len() + 2) / 3 * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(ALPHABET[(triple >> 18 & 0x3f) as usize] as char);
        out.push(ALPHABET[(triple >> 12 & 0x3f) as usize] as char);
        // 尾组缺字节补 '='：3 字节组出 4 字符，2 字节出 3+1'='，1 字节出 2+2'='
        out.push(if chunk.len() > 1 {
            ALPHABET[(triple >> 6 & 0x3f) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[(triple & 0x3f) as usize] as char
        } else {
            '='
        });
    }
    out
}

/// 严格解码标准 base64；任何非法字符、坏 padding、坏长度都报错。
pub fn decode(text: &str) -> Result<Vec<u8>, String> {
    let bytes = text.as_bytes();
    if bytes.len() % 4 != 0 {
        return Err(format!("base64 解码失败：长度 {} 不是 4 的倍数", bytes.len()));
    }
    let padding = bytes.iter().rev().take_while(|&&b| b == b'=').count();
    if padding > 2 {
        return Err("base64 解码失败：padding 最多两个 '='".to_string());
    }
    // '=' 只允许出现在末尾 padding 段，正文出现即拒绝
    if bytes[..bytes.len() - padding].contains(&b'=') {
        return Err("base64 解码失败：'=' 只能出现在末尾".to_string());
    }
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    for chunk in bytes.chunks(4) {
        let mut sextets = 0u32;
        for &byte in chunk {
            let value = if byte == b'=' { 0 } else { decode_value(byte)? };
            sextets = sextets << 6 | value as u32;
        }
        // padding 位置产生的低位垃圾位随截断丢弃，与 RFC 4648 解码一致
        out.push((sextets >> 16) as u8);
        if chunk[2] != b'=' {
            out.push((sextets >> 8) as u8);
        }
        if chunk[3] != b'=' {
            out.push(sextets as u8);
        }
    }
    Ok(out)
}

fn decode_value(byte: u8) -> Result<u8, String> {
    match byte {
        b'A'..=b'Z' => Ok(byte - b'A'),
        b'a'..=b'z' => Ok(byte - b'a' + 26),
        b'0'..=b'9' => Ok(byte - b'0' + 52),
        b'+' => Ok(62),
        b'/' => Ok(63),
        _ => Err(format!("base64 解码失败：非法字符 '{}'", byte as char)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_vectors() {
        assert_eq!(encode(b"foobar"), "Zm9vYmFy");
        assert_eq!(encode(b"foob"), "Zm9vYg==");
        assert_eq!(encode(b"fo"), "Zm8=");
        assert_eq!(encode(b""), "");
        assert_eq!(decode("Zm9vYmFy").unwrap(), b"foobar");
        assert_eq!(decode("Zm9vYg==").unwrap(), b"foob");
        assert_eq!(decode("Zm8=").unwrap(), b"fo");
        assert_eq!(decode("Zm9v").unwrap(), b"foo");
        assert_eq!(decode("").unwrap(), b"");
    }

    #[test]
    fn decode_rejects_invalid_input() {
        assert!(decode("!!").is_err());
        assert!(decode("!!!").is_err());
        assert!(decode("Zm9vYg=").is_err());
        assert!(decode("Zm9=vYg=").is_err());
        assert!(decode("Zm9vY").is_err());
    }

    #[test]
    fn roundtrip_all_byte_values() {
        let data: Vec<u8> = (0..=255u8).collect();
        assert_eq!(decode(&encode(&data)).unwrap(), data);
    }
}
