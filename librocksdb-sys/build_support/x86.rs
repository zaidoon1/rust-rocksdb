/// The Snappy define justified by the Rust x86 target features on MSVC.
///
/// Snappy's own MSVC fallback treats AVX2 as a proxy for BMI2, but Rust lets
/// callers enable either feature independently.
pub(crate) fn snappy_msvc_bmi2_define(target_features: &[impl AsRef<str>]) -> Option<&'static str> {
    target_features
        .iter()
        .any(|feature| feature.as_ref() == "bmi2")
        .then_some("SNAPPY_HAVE_BMI2")
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MacroDumpStyle {
    GnuLike,
    ClangCl,
}

/// Arguments that dump the predefined macros for an empty C++ input.
pub(crate) fn pclmul_macro_dump_args(style: MacroDumpStyle) -> [&'static str; 5] {
    let dump_macros = match style {
        MacroDumpStyle::GnuLike => "-dM",
        MacroDumpStyle::ClangCl => "/clang:-dM",
    };
    [dump_macros, "-E", "-x", "c++", "-"]
}

/// Whether RocksDB must compile out its three-way x86 CRC32C implementation.
pub(crate) fn should_disable_three_way_crc(
    pointer_width: u32,
    rust_has_pclmul: bool,
    compiler_has_pclmul: bool,
) -> bool {
    pointer_width != 64 || !(rust_has_pclmul || compiler_has_pclmul)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn avx2_without_bmi2_keeps_snappys_bmi2_path_off() {
        assert_eq!(snappy_msvc_bmi2_define(&["avx2"]), None);
    }

    #[test]
    fn bmi2_enables_snappys_bmi2_path_without_avx2() {
        assert_eq!(snappy_msvc_bmi2_define(&["bmi2"]), Some("SNAPPY_HAVE_BMI2"));
    }

    #[test]
    fn clang_cl_escapes_the_macro_dump_option() {
        assert_eq!(
            pclmul_macro_dump_args(MacroDumpStyle::ClangCl),
            ["/clang:-dM", "-E", "-x", "c++", "-"]
        );
    }

    #[test]
    fn gnu_like_compilers_take_the_plain_macro_dump_option() {
        assert_eq!(
            pclmul_macro_dump_args(MacroDumpStyle::GnuLike),
            ["-dM", "-E", "-x", "c++", "-"]
        );
    }

    #[test]
    fn three_way_crc_requires_a_64_bit_target() {
        assert!(should_disable_three_way_crc(32, true, true));
    }

    #[test]
    fn x64_without_pclmul_disables_three_way_crc() {
        assert!(should_disable_three_way_crc(64, false, false));
    }

    #[test]
    fn rust_pclmul_enables_three_way_crc_on_x64() {
        assert!(!should_disable_three_way_crc(64, true, false));
    }

    #[test]
    fn compiler_pclmul_enables_three_way_crc_on_x64() {
        assert!(!should_disable_three_way_crc(64, false, true));
    }
}
