use mime::Mime;

use crate::error::{ValidationError, ValidationErrorKind};

/// Parse a `Content-Type` header value or a spec media type into a [`Mime`].
///
/// Parameters such as `charset` are retained on the value but ignored when
/// matching. Returns `None` for a malformed value.
pub fn parse_media_type(value: &str) -> Option<Mime> {
    value.trim().parse().ok()
}

/// Whether a concrete request media type satisfies a media type from the spec.
///
/// `expected` may be a wildcard such as `*/*` or `application/*`. Type and
/// subtype are compared case-insensitively and parameters are ignored.
pub fn media_type_matches(expected: &Mime, actual: &Mime) -> bool {
    if expected.type_() == mime::STAR {
        return true;
    }
    if !expected
        .type_()
        .as_str()
        .eq_ignore_ascii_case(actual.type_().as_str())
    {
        return false;
    }
    expected.subtype() == mime::STAR
        || expected
            .subtype()
            .as_str()
            .eq_ignore_ascii_case(actual.subtype().as_str())
}

/// Check that the request `Content-Type` matches one of the spec's media types.
pub fn validate_content_type(
    request_content_type: Option<&str>,
    expected_media_types: &[&Mime],
    errors: &mut Vec<ValidationError>,
) {
    if expected_media_types.is_empty() {
        return;
    }

    let expected_list = || {
        expected_media_types
            .iter()
            .map(|m| m.essence_str())
            .collect::<Vec<_>>()
            .join(", ")
    };

    let Some(raw) = request_content_type else {
        errors.push(ValidationError {
            kind: ValidationErrorKind::UnsupportedContentType,
            message: format!(
                "Missing Content-Type header. Expected one of: {}",
                expected_list()
            ),
            path: "header.Content-Type".to_string(),
        });
        return;
    };

    let Some(actual) = parse_media_type(raw) else {
        errors.push(ValidationError {
            kind: ValidationErrorKind::UnsupportedContentType,
            message: format!(
                "Malformed Content-Type '{}'. Expected one of: {}",
                raw.trim(),
                expected_list()
            ),
            path: "header.Content-Type".to_string(),
        });
        return;
    };

    if !expected_media_types
        .iter()
        .any(|expected| media_type_matches(expected, &actual))
    {
        errors.push(ValidationError {
            kind: ValidationErrorKind::UnsupportedContentType,
            message: format!(
                "Content-Type '{}' is not supported. Expected one of: {}",
                actual.essence_str(),
                expected_list()
            ),
            path: "header.Content-Type".to_string(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(request: Option<&str>, expected: &[&str]) -> Vec<ValidationError> {
        let parsed: Vec<Mime> = expected.iter().map(|m| m.parse().unwrap()).collect();
        let refs: Vec<&Mime> = parsed.iter().collect();
        let mut errors = Vec::new();
        validate_content_type(request, &refs, &mut errors);
        errors
    }

    #[test]
    fn test_exact_match() {
        assert!(check(Some("application/json"), &["application/json"]).is_empty());
    }

    #[test]
    fn test_match_with_charset() {
        assert!(check(
            Some("application/json; charset=utf-8"),
            &["application/json"]
        )
        .is_empty());
    }

    #[test]
    fn test_match_is_case_insensitive() {
        assert!(check(Some("Application/JSON"), &["application/json"]).is_empty());
    }

    #[test]
    fn test_wildcard_match() {
        assert!(check(Some("application/json"), &["application/*"]).is_empty());
        assert!(check(Some("text/plain"), &["*/*"]).is_empty());
        assert!(!check(Some("text/plain"), &["application/*"]).is_empty());
    }

    #[test]
    fn test_structured_suffix_is_not_a_match() {
        // OpenAPI media types are matched exactly; +json suffixes are distinct types.
        assert!(!check(Some("application/vnd.api+json"), &["application/json"]).is_empty());
    }

    #[test]
    fn test_no_match() {
        let errors = check(Some("text/plain"), &["application/json"]);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].kind, ValidationErrorKind::UnsupportedContentType);
    }

    #[test]
    fn test_missing_content_type() {
        assert_eq!(check(None, &["application/json"]).len(), 1);
    }

    #[test]
    fn test_malformed_content_type() {
        let errors = check(Some("json"), &["application/json"]);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].message.starts_with("Malformed Content-Type"));
    }

    #[test]
    fn test_empty_expected_skips() {
        assert!(check(Some("anything"), &[]).is_empty());
    }
}
