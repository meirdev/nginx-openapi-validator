use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct ValidationError {
    pub kind: ValidationErrorKind,
    pub message: String,
    /// JSON pointer or parameter name indicating where the error occurred.
    pub path: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ValidationErrorKind {
    PathNotFound,
    MethodNotAllowed,
    MissingRequiredParam,
    InvalidParamValue,
    UnsupportedContentType,
    MissingRequiredBody,
    InvalidBody,
    SchemaValidation,
}

impl ValidationErrorKind {
    /// Maps validation error kind to an HTTP status code.
    pub fn http_status(&self) -> u16 {
        match self {
            Self::PathNotFound => 404,
            Self::MethodNotAllowed => 405,
            Self::UnsupportedContentType => 415,
            _ => 400,
        }
    }
}

#[derive(Debug)]
pub enum ValidationResult {
    Valid,
    Invalid(Vec<ValidationError>),
}

impl ValidationResult {
    pub fn is_valid(&self) -> bool {
        matches!(self, Self::Valid)
    }

    /// Returns the most appropriate HTTP status code for the validation errors.
    /// Uses the first error's status code.
    pub fn http_status(&self) -> Option<u16> {
        match self {
            Self::Valid => None,
            Self::Invalid(errors) => errors.first().map(|e| e.kind.http_status()),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SpecError {
    #[error("failed to parse OpenAPI spec: {0}")]
    ParseError(String),
    #[error("invalid path template '{0}': {1}")]
    PathTemplateError(String, String),
    #[error("failed to resolve reference: {0}")]
    RefResolutionError(String),
    #[error("failed to compile JSON schema: {0}")]
    SchemaCompileError(String),
}
