//! Aperture write convergence (issue 1452) — RED stubs: the signatures the tests pin, with no
//! behaviour yet (no correction, never settled, motion unknown, the report unchanged).

/// RED stub (issue 1452): never corrects.
pub fn aperture_correction(
    _requested: usize,
    _before: Option<usize>,
    _landed: Option<usize>,
    _n: usize,
) -> Option<usize> {
    None
}

/// RED stub (issue 1452): never settles.
pub fn aperture_settle(_before_raw: Option<i64>, _reads: &[i64], _max_reads: usize) -> Option<i64> {
    None
}

/// RED stub (issue 1452): motion unknown.
pub fn aperture_moved(_before_raw: Option<i64>, _landed_raw: Option<i64>) -> Option<bool> {
    None
}

/// RED stub (issue 1452): the report unchanged.
pub fn not_applied_after_aperture_motion(
    not_applied: Vec<String>,
    _aperture_moved: Option<bool>,
) -> Vec<String> {
    not_applied
}

/// RED stub (issue 1452): no index.
pub fn fnumber_label_index(_labels: &[String], _raw_x100: i64) -> Option<usize> {
    None
}

/// RED stub (issue 1452): no value.
pub fn fnumber_raw_from_block(_block: &str) -> Option<i64> {
    None
}

/// RED stub (issue 1452): no value.
pub fn landed_aperture(_raw_x100: i64, _labels: &[String]) -> (Option<f64>, Option<f64>) {
    (None, None)
}
