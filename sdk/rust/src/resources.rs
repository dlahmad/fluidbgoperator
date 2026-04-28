use sha2::{Digest, Sha256};

const MAX_DERIVED_QUEUE_NAME_LEN: usize = 63;
const STABLE_SUFFIX_LEN: usize = 16;

pub fn derived_temp_queue_name(
    namespace: &str,
    blue_green_ref: &str,
    inception_point: &str,
    role: &str,
    logical: &str,
) -> String {
    derived_temp_queue_name_with_uid(
        namespace,
        blue_green_ref,
        "",
        inception_point,
        role,
        logical,
    )
}

pub fn derived_temp_queue_name_with_uid(
    namespace: &str,
    blue_green_ref: &str,
    blue_green_uid: &str,
    inception_point: &str,
    role: &str,
    logical: &str,
) -> String {
    let suffix = stable_suffix(&[
        namespace,
        blue_green_ref,
        blue_green_uid,
        inception_point,
        role,
        logical,
    ]);
    format!("fluidbg-{}-{suffix}", temp_queue_kind(logical))
}

pub fn derived_shadow_queue_name(temp_queue_name: &str, shadow_suffix: &str) -> String {
    let shadow_suffix = sanitize_shadow_suffix(shadow_suffix);
    if shadow_suffix
        .chars()
        .all(|ch| ch == '-' || ch == '_' || ch == '.')
    {
        temp_queue_name.to_string()
    } else if temp_queue_name.chars().count() + shadow_suffix.chars().count()
        <= MAX_DERIVED_QUEUE_NAME_LEN
    {
        format!("{temp_queue_name}{shadow_suffix}")
    } else if let Some((prefix, suffix)) = stable_suffix_from_name(temp_queue_name) {
        let budget = MAX_DERIVED_QUEUE_NAME_LEN
            .saturating_sub(shadow_suffix.chars().count() + 1 + suffix.len())
            .max(1);
        let prefix = prefix
            .trim_end_matches(['-', '_', '.'])
            .chars()
            .take(budget)
            .collect::<String>();
        format!("{prefix}{shadow_suffix}-{suffix}")
    } else {
        let budget = MAX_DERIVED_QUEUE_NAME_LEN.saturating_sub(shadow_suffix.chars().count());
        let base = temp_queue_name.chars().take(budget).collect::<String>();
        format!("{base}{shadow_suffix}")
    }
}

fn stable_suffix_from_name(name: &str) -> Option<(&str, &str)> {
    let (prefix, suffix) = name.rsplit_once('-')?;
    if suffix.len() == STABLE_SUFFIX_LEN && suffix.chars().all(|ch| ch.is_ascii_hexdigit()) {
        Some((prefix, suffix))
    } else {
        None
    }
}

fn temp_queue_kind(logical: &str) -> &'static str {
    match logical {
        "green-input" => "green-in",
        "blue-input" => "blue-in",
        "green-output" => "green-out",
        "blue-output" => "blue-out",
        _ => "tmp",
    }
}

pub fn derived_scoped_identity_name(
    prefix: &str,
    namespace: &str,
    blue_green_ref: &str,
    blue_green_uid: &str,
    inception_point: &str,
    max_len: usize,
) -> String {
    let suffix = stable_suffix(&[namespace, blue_green_ref, blue_green_uid, inception_point]);
    let safe_prefix = sanitize(prefix);
    let budget = max_len.saturating_sub(suffix.len() + 2).max(1);
    let readable = sanitize(&format!("{namespace}-{blue_green_ref}-{inception_point}"));
    let readable = readable.chars().take(budget).collect::<String>();
    format!("{safe_prefix}-{readable}-{suffix}")
        .chars()
        .take(max_len)
        .collect()
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
    use super::{
        derived_scoped_identity_name, derived_shadow_queue_name, derived_temp_queue_name,
        derived_temp_queue_name_with_uid,
    };

    #[test]
    fn derived_temp_names_are_scoped_by_namespace_and_bgd() {
        let a = derived_temp_queue_name("ns-a", "bgd", "incoming", "splitter", "blue-input");
        let b = derived_temp_queue_name("ns-b", "bgd", "incoming", "splitter", "blue-input");
        let c = derived_temp_queue_name("ns-a", "other", "incoming", "splitter", "blue-input");

        assert_ne!(a, b);
        assert_ne!(a, c);
        assert!(a.starts_with("fluidbg-blue-in-"));
        assert!(a.len() <= 63);
        assert!(!a.contains("bgd"));
        assert!(!a.contains("incoming"));
    }

    #[test]
    fn derived_temp_names_are_scoped_by_bgd_uid_when_available() {
        let a =
            derived_temp_queue_name_with_uid("ns", "bgd", "uid-a", "incoming", "splitter", "blue");
        let b =
            derived_temp_queue_name_with_uid("ns", "bgd", "uid-b", "incoming", "splitter", "blue");

        assert_ne!(a, b);
    }

    #[test]
    fn derived_shadow_names_are_suffixes_of_real_queue_names() {
        let shadow = derived_shadow_queue_name("fluidbg-blue-in-1234567890abcdef", "_dlq");

        assert_eq!(shadow, "fluidbg-blue-in-1234567890abcdef_dlq");
    }

    #[test]
    fn derived_shadow_names_keep_suffix_for_max_length_temp_names() {
        let temp = "fluidbg-green-in-this-prefix-is-intentionallyx-1234567890abcdef";
        assert_eq!(temp.len(), 63);
        let stable_suffix = temp.rsplit_once('-').unwrap().1;

        let shadow = derived_shadow_queue_name(temp, "_dlq");

        assert!(shadow.ends_with(&format!("_dlq-{stable_suffix}")));
        assert_ne!(shadow, temp);
        assert!(shadow.len() <= 63);
    }

    #[test]
    fn derived_shadow_names_are_bounded_for_user_base_queues() {
        let shadow = derived_shadow_queue_name(&"basequeue".repeat(10), ".deadletter");

        assert!(shadow.ends_with(".deadletter"));
        assert!(shadow.len() <= 63);
    }

    #[test]
    fn scoped_identity_names_are_bounded_and_stable() {
        let name = derived_scoped_identity_name(
            "fbg-rmq",
            "very-long-namespace-name",
            "very-long-blue-green-deployment-name",
            "uid-123",
            "very-long-inception-point-name",
            48,
        );

        assert!(name.starts_with("fbg-rmq-"));
        assert!(name.len() <= 48);
        assert_eq!(
            name,
            derived_scoped_identity_name(
                "fbg-rmq",
                "very-long-namespace-name",
                "very-long-blue-green-deployment-name",
                "uid-123",
                "very-long-inception-point-name",
                48,
            )
        );
    }
}
