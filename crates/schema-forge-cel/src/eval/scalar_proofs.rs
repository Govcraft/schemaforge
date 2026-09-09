//! Each harness calls the production helper, without replacing its body.
use super::*;

#[kani::proof_for_contract(i64_add)]
#[kani::solver(z3)]
fn i64_add_contract() {
    let _ = i64_add(kani::any(), kani::any());
}

#[kani::proof_for_contract(i64_sub)]
#[kani::solver(z3)]
fn i64_sub_contract() {
    let _ = i64_sub(kani::any(), kani::any());
}

#[kani::proof_for_contract(i64_mul)]
#[kani::solver(z3)]
fn i64_mul_contract() {
    let _ = i64_mul(kani::any(), kani::any());
}

#[kani::proof_for_contract(u64_add)]
#[kani::solver(z3)]
fn u64_add_contract() {
    let _ = u64_add(kani::any(), kani::any());
}

#[kani::proof_for_contract(u64_sub)]
#[kani::solver(z3)]
fn u64_sub_contract() {
    let _ = u64_sub(kani::any(), kani::any());
}

#[kani::proof_for_contract(u64_mul)]
#[kani::solver(z3)]
fn u64_mul_contract() {
    let _ = u64_mul(kani::any(), kani::any());
}

#[kani::proof_for_contract(u64_div)]
#[kani::solver(z3)]
fn u64_div_contract() {
    let _ = u64_div(kani::any(), kani::any());
}

#[kani::proof_for_contract(u64_rem)]
#[kani::solver(z3)]
fn u64_rem_contract() {
    let _ = u64_rem(kani::any(), kani::any());
}

#[kani::proof_for_contract(i64_neg)]
#[kani::solver(z3)]
fn i64_neg_contract() {
    let _ = i64_neg(kani::any());
}

#[kani::proof_for_contract(uint_to_int)]
#[kani::solver(z3)]
fn uint_to_int_contract() {
    let _ = uint_to_int(kani::any());
}

#[kani::proof_for_contract(int_to_uint)]
#[kani::solver(z3)]
fn int_to_uint_contract() {
    let _ = int_to_uint(kani::any());
}

#[kani::proof_for_contract(int_to_double)]
fn int_to_double_contract() {
    let _ = int_to_double(kani::any());
}

#[kani::proof_for_contract(uint_to_double)]
fn uint_to_double_contract() {
    let _ = uint_to_double(kani::any());
}

#[kani::proof_for_contract(double_to_int)]
fn double_to_int_contract() {
    let _ = double_to_int(f64::from_bits(kani::any()));
}

#[kani::proof_for_contract(double_to_uint)]
fn double_to_uint_contract() {
    let _ = double_to_uint(f64::from_bits(kani::any()));
}

#[kani::proof_for_contract(int_uint_cmp)]
#[kani::solver(z3)]
fn mixed_comparison_contract() {
    let _ = int_uint_cmp(kani::any(), kani::any());
}

#[kani::proof_for_contract(duration_nanos)]
#[kani::solver(z3)]
fn duration_grouping_contract() {
    let _ = duration_nanos(kani::any(), kani::any());
}

#[kani::proof_for_contract(duration_sum)]
#[kani::solver(z3)]
fn duration_arithmetic_contract() {
    let _ = duration_sum(kani::any(), kani::any(), kani::any());
}

#[kani::proof_for_contract(timestamp_sum)]
#[kani::solver(z3)]
fn timestamp_arithmetic_contract() {
    let _ = timestamp_sum(kani::any(), kani::any(), kani::any());
}

fn nanos(d: TimeDelta) -> i128 {
    d.num_seconds() as i128 * 1_000_000_000 + d.subsec_nanos() as i128
}

fn check_duration_result(result: Option<TimeDelta>, expected: i128) {
    if expected >= i64::MIN as i128 && expected <= i64::MAX as i128 {
        assert!(result.is_some());
        assert_eq!(nanos(result.unwrap()), expected);
    } else {
        assert!(result.is_none());
    }
}

#[kani::proof]
#[kani::solver(z3)]
fn duration_adapter_boundaries() {
    let choice: u8 = kani::any();
    kani::assume(choice < 5);
    let x = match choice {
        0 => TimeDelta::MIN,
        1 => TimeDelta::MAX,
        2 => TimeDelta::nanoseconds(i64::MIN),
        3 => TimeDelta::nanoseconds(i64::MAX),
        _ => TimeDelta::zero(),
    };
    let y = TimeDelta::nanoseconds(kani::any::<i8>() as i64);
    check_duration_result(duration_add(x, y), nanos(x) + nanos(y));
    check_duration_result(duration_sub(x, y), nanos(x) - nanos(y));
}

fn timestamp_nanos(t: DateTime<Utc>) -> i128 {
    t.timestamp() as i128 * 1_000_000_000 + t.timestamp_subsec_nanos() as i128
}

fn check_timestamp_result(result: Option<DateTime<Utc>>, expected: i128) {
    let min = -62_135_596_800_i128 * 1_000_000_000;
    let max = 253_402_300_799_i128 * 1_000_000_000 + 999_999_999;
    if (min..=max).contains(&expected) {
        assert!(result.is_some());
        assert_eq!(timestamp_nanos(result.unwrap()), expected);
    } else {
        assert!(result.is_none());
    }
}

fn check_timestamp_boundary(t: DateTime<Utc>) {
    let d = TimeDelta::nanoseconds(kani::any::<i8>() as i64);
    check_timestamp_result(timestamp_add(t, d), timestamp_nanos(t) + nanos(d));
    check_timestamp_result(timestamp_sub(t, d), timestamp_nanos(t) - nanos(d));
}

#[kani::proof]
#[kani::solver(z3)]
fn timestamp_cel_min_boundary() {
    check_timestamp_boundary(DateTime::from_timestamp(-62_135_596_800, 0).unwrap());
}

#[kani::proof]
#[kani::solver(z3)]
fn timestamp_cel_max_boundary() {
    check_timestamp_boundary(DateTime::from_timestamp(253_402_300_799, 999_999_999).unwrap());
}

#[kani::proof]
#[kani::solver(z3)]
fn timestamp_chrono_min_boundary() {
    check_timestamp_boundary(DateTime::<Utc>::MIN_UTC);
}

#[kani::proof]
#[kani::solver(z3)]
fn timestamp_chrono_max_boundary() {
    check_timestamp_boundary(DateTime::<Utc>::MAX_UTC);
}

// A wider truncated integer model avoids the production comparator's f64
// precision fast path and its floor/ceil boundary algorithm.
fn compare_integer_double(integer: i128, d: f64) -> Option<Ordering> {
    if d.is_nan() {
        return None;
    }
    if d >= 36_893_488_147_419_103_232.0 {
        return Some(Ordering::Less);
    }
    if d <= -36_893_488_147_419_103_232.0 {
        return Some(Ordering::Greater);
    }
    let truncated = d as i128;
    Some(match integer.cmp(&truncated) {
        Ordering::Equal if d > d.trunc() => Ordering::Less,
        Ordering::Equal if d < d.trunc() => Ordering::Greater,
        ordering => ordering,
    })
}

#[kani::proof]
fn int_double_comparison_full_range() {
    let integer: i64 = kani::any();
    let double = f64::from_bits(kani::any());
    assert_eq!(
        int_double_cmp(integer, double),
        compare_integer_double(integer as i128, double)
    );
}

#[kani::proof]
fn uint_double_comparison_full_range() {
    let integer: u64 = kani::any();
    let double = f64::from_bits(kani::any());
    assert_eq!(
        uint_double_cmp(integer, double),
        compare_integer_double(integer as i128, double)
    );
}

#[kani::proof_for_contract(i64_div)]
#[kani::solver(z3)]
fn i64_div_contract() {
    let _ = i64_div(kani::any(), kani::any());
}

#[kani::proof_for_contract(i64_rem)]
#[kani::solver(z3)]
fn i64_rem_contract() {
    let _ = i64_rem(kani::any(), kani::any());
}
