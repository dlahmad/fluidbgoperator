pub fn namespace_of(field: &str) -> &str {
    field.split('.').next().unwrap_or(field)
}

pub fn is_body_field(field: &str) -> bool {
    field.ends_with(".body")
}

pub fn split_header_or_property(field: &str) -> Option<(&str, &str)> {
    let parts: Vec<&str> = field.splitn(3, '.').collect();
    if parts.len() >= 3 {
        Some((parts[0], &field[parts[0].len() + parts[1].len() + 2..]))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn namespace_http() {
        assert_eq!(namespace_of("http.method"), "http");
        assert_eq!(namespace_of("http.header.X-Request-Id"), "http");
        assert_eq!(namespace_of("http.body"), "http");
    }

    #[test]
    fn namespace_queue() {
        assert_eq!(namespace_of("queue.body"), "queue");
        assert_eq!(namespace_of("queue.property.correlationId"), "queue");
    }

    #[test]
    fn body_fields() {
        assert!(is_body_field("http.body"));
        assert!(is_body_field("queue.body"));
        assert!(!is_body_field("http.method"));
        assert!(!is_body_field("queue.property.correlationId"));
    }

    #[test]
    fn split_header() {
        let (ns, key) = split_header_or_property("http.header.X-Request-Id").unwrap();
        assert_eq!(ns, "http");
        assert_eq!(key, "X-Request-Id");
    }

    #[test]
    fn split_property() {
        let (ns, key) = split_header_or_property("queue.property.correlationId").unwrap();
        assert_eq!(ns, "queue");
        assert_eq!(key, "correlationId");
    }

    #[test]
    fn split_simple_field_returns_none() {
        assert!(split_header_or_property("http.method").is_none());
        assert!(split_header_or_property("http.body").is_none());
    }
}
