use sha2::{Digest, Sha256};

pub fn derived_temp_queue_name(
    namespace: &str,
    blue_green_ref: &str,
    inception_point: &str,
    role: &str,
    logical: &str,
) -> String {
    let suffix = stable_suffix(&[namespace, blue_green_ref, inception_point, role, logical]);
    format!("fluidbg-{}-{suffix}", sanitize(logical))
}

fn stable_suffix(parts: &[&str]) -> String {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update(part.len().to_le_bytes());
        hasher.update(part.as_bytes());
    }
    hasher
        .finalize()
        .iter()
        .flat_map(|byte| {
            let hi = byte >> 4;
            let lo = byte & 0x0f;
            [
                char::from_digit(hi.into(), 16),
                char::from_digit(lo.into(), 16),
            ]
        })
        .flatten()
        .take(16)
        .collect()
}

fn sanitize(value: &str) -> String {
    let mut out = String::new();
    let mut last_dash = false;
    for ch in value.chars() {
        let ch = ch.to_ascii_lowercase();
        if ch.is_ascii_alphanumeric() {
            out.push(ch);
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    let trimmed = out.trim_matches('-');
    if trimmed.is_empty() {
        "queue".to_string()
    } else {
        trimmed.chars().take(38).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::derived_temp_queue_name;

    #[test]
    fn derived_temp_names_are_scoped_by_namespace_and_bgd() {
        let a = derived_temp_queue_name("ns-a", "bgd", "incoming", "splitter", "blue-input");
        let b = derived_temp_queue_name("ns-b", "bgd", "incoming", "splitter", "blue-input");
        let c = derived_temp_queue_name("ns-a", "other", "incoming", "splitter", "blue-input");

        assert_ne!(a, b);
        assert_ne!(a, c);
        assert!(a.starts_with("fluidbg-blue-input-"));
        assert!(a.len() <= 63);
    }
}
