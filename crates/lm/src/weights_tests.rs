//! 重排权重方案的解析与门限测试。

use super::half_to_f32;

#[test]
fn converts_representative_halves() {
    assert_eq!(half_to_f32(0x0000), 0.0);
    assert_eq!(half_to_f32(0x8000), -0.0);
    assert_eq!(half_to_f32(0x3c00), 1.0);
    assert_eq!(half_to_f32(0xc000), -2.0);
    assert_eq!(half_to_f32(0x3555), 0.333_251_95);
    // Subnormals, which the renormalizing branch has to reach. A subnormal half is exactly
    // `mantissa * 2^-24`, and every such value is representable in f32, so these are exact.
    assert_eq!(half_to_f32(0x03ff), 1023.0 / 16_777_216.0);
    assert_eq!(half_to_f32(0x0001), 1.0 / 16_777_216.0);
    assert_eq!(half_to_f32(0x0200), 512.0 / 16_777_216.0);
    assert_eq!(half_to_f32(0x8001), -1.0 / 16_777_216.0);
    assert!(half_to_f32(0x7c00).is_infinite());
    assert!(half_to_f32(0x7e00).is_nan());
}
