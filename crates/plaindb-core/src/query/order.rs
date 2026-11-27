use std::cmp::Ordering;

use serde_json::{Map, Number, Value};

fn rank(value: &Value) -> u8 {
    match value {
        Value::Null => 0,
        Value::Bool(_) => 1,
        Value::Number(_) => 2,
        Value::String(_) => 3,
        Value::Array(_) => 4,
        Value::Object(_) => 5,
    }
}

pub fn compare(left: &Value, right: &Value) -> Ordering {
    match (left, right) {
        (Value::Bool(a), Value::Bool(b)) => a.cmp(b),
        (Value::Number(a), Value::Number(b)) => compare_numbers(a, b),
        (Value::String(a), Value::String(b)) => a.cmp(b),
        (Value::Array(a), Value::Array(b)) => compare_arrays(a, b),
        (Value::Object(a), Value::Object(b)) => compare_objects(a, b),
        _ => rank(left).cmp(&rank(right)),
    }
}

fn integer(number: &Number) -> Option<i128> {
    number
        .as_i64()
        .map(i128::from)
        .or_else(|| number.as_u64().map(i128::from))
}

fn float(number: &Number) -> f64 {
    let value = number.as_f64().unwrap_or(f64::NAN);
    if value == 0.0 { 0.0 } else { value }
}

const I128_BOUND: f64 = 170_141_183_460_469_231_731_687_303_715_884_105_728.0;

fn compare_integer_to_float(integer: i128, float: f64) -> Ordering {
    if float.is_nan() || float >= I128_BOUND {
        return Ordering::Less;
    }
    if float < -I128_BOUND {
        return Ordering::Greater;
    }
    #[allow(clippy::cast_possible_truncation)]
    let whole = float.trunc() as i128;
    integer
        .cmp(&whole)
        .then_with(|| 0.0_f64.total_cmp(&float.fract()))
}

fn compare_numbers(left: &Number, right: &Number) -> Ordering {
    match (integer(left), integer(right)) {
        (Some(a), Some(b)) => a.cmp(&b),
        (Some(a), None) => compare_integer_to_float(a, float(right)),
        (None, Some(b)) => compare_integer_to_float(b, float(left)).reverse(),
        (None, None) => float(left).total_cmp(&float(right)),
    }
}

fn compare_arrays(left: &[Value], right: &[Value]) -> Ordering {
    left.iter()
        .zip(right)
        .map(|(a, b)| compare(a, b))
        .find(|ordering| ordering.is_ne())
        .unwrap_or_else(|| left.len().cmp(&right.len()))
}

fn sorted_entries(map: &Map<String, Value>) -> Vec<(&String, &Value)> {
    let mut entries: Vec<_> = map.iter().collect();
    entries.sort_unstable_by(|a, b| a.0.cmp(b.0));
    entries
}

fn compare_objects(left: &Map<String, Value>, right: &Map<String, Value>) -> Ordering {
    let (left_entries, right_entries) = (sorted_entries(left), sorted_entries(right));
    left_entries
        .iter()
        .zip(&right_entries)
        .map(|((a_key, a_value), (b_key, b_value))| {
            a_key.cmp(b_key).then_with(|| compare(a_value, b_value))
        })
        .find(|ordering| ordering.is_ne())
        .unwrap_or_else(|| left_entries.len().cmp(&right_entries.len()))
}

#[derive(Debug, Clone)]
pub struct Key(pub Value);

impl PartialEq for Key {
    fn eq(&self, other: &Self) -> bool {
        compare(&self.0, &other.0) == Ordering::Equal
    }
}

impl Eq for Key {}

impl PartialOrd for Key {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Key {
    fn cmp(&self, other: &Self) -> Ordering {
        compare(&self.0, &other.0)
    }
}
