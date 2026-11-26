use serde_json::{Map, Value};

pub fn sort_keys(value: &Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(sort_map(map)),
        Value::Array(items) => Value::Array(items.iter().map(sort_keys).collect()),
        other => other.clone(),
    }
}

pub fn sort_map(map: &Map<String, Value>) -> Map<String, Value> {
    let mut keys: Vec<&String> = map.keys().collect();
    keys.sort();
    let mut sorted = Map::new();
    for key in keys {
        sorted.insert(key.clone(), sort_keys(&map[key]));
    }
    sorted
}

pub fn pretty_bytes(value: &Value) -> Vec<u8> {
    let mut bytes = serde_json::to_vec_pretty(&sort_keys(value)).expect("a json value serialises");
    bytes.push(b'\n');
    bytes
}
