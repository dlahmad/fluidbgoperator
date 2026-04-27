use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

pub fn routes_to_blue(payload: &[u8], traffic_percent: u8) -> bool {
    if traffic_percent == 0 {
        return false;
    }
    if traffic_percent >= 100 {
        return true;
    }
    let mut hasher = DefaultHasher::new();
    payload.hash(&mut hasher);
    (hasher.finish() % 100) < traffic_percent as u64
}
