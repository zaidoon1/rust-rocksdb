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
}
