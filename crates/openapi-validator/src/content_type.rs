use crate::error::{ValidationError, ValidationErrorKind};

/// Check if the request content type matches any of the expected media types.
///
/// Supports exact matches and wildcard patterns like `*/*` and `application/*`.
pub fn validate_content_type(
    request_content_type: Option<&str>,
    expected_media_types: &[&str],
    errors: &mut Vec<ValidationError>,
) {
    if expected_media_types.is_empty() {
        return;
    }

    let request_ct = match request_content_type {
        Some(ct) => ct,
        None => {
            errors.push(ValidationError {
                kind: ValidationErrorKind::UnsupportedContentType,
                message: format!(
                    "Missing Content-Type header. Expected one of: {}",
                    expected_media_types.join(", ")
                ),
                path: "header.Content-Type".to_string(),
            });
            return;
        }
    };

    // Extract the media type without parameters (e.g., charset)
    let request_media_type = request_ct
        .split(';')
        .next()
        .unwrap_or(request_ct)
        .trim();

    let matches = expected_media_types.iter().any(|expected| {
        if *expected == "*/*" {
            return true;
        }
        if expected.eq_ignore_ascii_case(request_media_type) {
            return true;
        }
        // Check wildcard like "application/*"
        if let Some(prefix) = expected.strip_suffix("/*") {
            if let Some(req_prefix) = request_media_type.split('/').next() {
                return prefix.eq_ignore_ascii_case(req_prefix);
            }
        }
        false
    });

    if !matches {
        errors.push(ValidationError {
            kind: ValidationErrorKind::UnsupportedContentType,
            message: format!(
                "Content-Type '{}' is not supported. Expected one of: {}",
                request_media_type,
                expected_media_types.join(", ")
            ),
            path: "header.Content-Type".to_string(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_exact_match() {
        let mut errors = Vec::new();
        validate_content_type(Some("application/json"), &["application/json"], &mut errors);
        assert!(errors.is_empty());
    }

    #[test]
    fn test_match_with_charset() {
        let mut errors = Vec::new();
        validate_content_type(
            Some("application/json; charset=utf-8"),
            &["application/json"],
            &mut errors,
        );
        assert!(errors.is_empty());
    }

    #[test]
    fn test_wildcard_match() {
        let mut errors = Vec::new();
        validate_content_type(Some("application/json"), &["application/*"], &mut errors);
        assert!(errors.is_empty());
    }

    #[test]
    fn test_no_match() {
        let mut errors = Vec::new();
        validate_content_type(Some("text/plain"), &["application/json"], &mut errors);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].kind, ValidationErrorKind::UnsupportedContentType);
    }

    #[test]
    fn test_missing_content_type() {
        let mut errors = Vec::new();
        validate_content_type(None, &["application/json"], &mut errors);
        assert_eq!(errors.len(), 1);
    }

    #[test]
    fn test_empty_expected_skips() {
        let mut errors = Vec::new();
        validate_content_type(Some("anything"), &[], &mut errors);
        assert!(errors.is_empty());
    }
}
