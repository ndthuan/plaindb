use std::cmp::Ordering;

use serde_json::Value;

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
        (Value::Number(a), Value::Number(b)) => a
            .as_f64()
            .unwrap_or(f64::NAN)
            .total_cmp(&b.as_f64().unwrap_or(f64::NAN)),
        (Value::String(a), Value::String(b)) => a.cmp(b),
        (Value::Array(_), Value::Array(_)) | (Value::Object(_), Value::Object(_)) => {
            left.to_string().cmp(&right.to_string())
        }
        _ => rank(left).cmp(&rank(right)),
    }
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
