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

pub fn derived_shadow_queue_name(temp_queue_name: &str, shadow_suffix: &str) -> String {
    let shadow_suffix = sanitize_shadow_suffix(shadow_suffix);
    if shadow_suffix
        .chars()
        .all(|ch| ch == '-' || ch == '_' || ch == '.')
    {
        temp_queue_name.to_string()
    } else {
        format!("{temp_queue_name}{shadow_suffix}")
            .chars()
            .take(63)
            .collect()
    }
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

fn sanitize_shadow_suffix(value: &str) -> String {
    value
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
        .take(24)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{derived_shadow_queue_name, derived_temp_queue_name};

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

    #[test]
    fn derived_shadow_names_are_suffixes_of_real_queue_names() {
        let shadow = derived_shadow_queue_name("fluidbg-blue-input-1234567890abcdef", "_dlq");

        assert_eq!(shadow, "fluidbg-blue-input-1234567890abcdef_dlq");
    }
}
