//! 标准 Base64(RFC 4648):Basic 认证的头要用它。
//!
//! 平台无关的小工具,联网层与设置窗共用;没有依赖,单元测试直接跑。

/// 编码:每 3 字节一组,不足的补 `=`。
pub fn encode(data: &[u8]) -> String {
    const ALPHABET: &[u8; 64] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let mut bits = [0u8; 3];
        bits[..chunk.len()].copy_from_slice(chunk);
        let group = u32::from(bits[0]) << 16 | u32::from(bits[1]) << 8 | u32::from(bits[2]);
        out.push(ALPHABET[(group >> 18 & 0x3f) as usize] as char);
        out.push(ALPHABET[(group >> 12 & 0x3f) as usize] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(group >> 6 & 0x3f) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[(group & 0x3f) as usize] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::encode;

    #[test]
    fn matches_rfc4648_vectors() {
        assert_eq!(encode(b""), "");
        assert_eq!(encode(b"f"), "Zg==");
        assert_eq!(encode(b"fo"), "Zm8=");
        assert_eq!(encode(b"foo"), "Zm9v");
        assert_eq!(encode(b"foob"), "Zm9vYg==");
        assert_eq!(encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(encode(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn encodes_basic_credentials() {
        assert_eq!(encode(b"alice:s3cret"), "YWxpY2U6czNjcml0");
    }

    #[test]
    fn handles_multibyte_text() {
        assert_eq!(encode("café".as_bytes()), "Y2Fmw6k=");
    }
}
