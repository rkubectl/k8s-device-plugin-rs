use super::*;

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ValidationError {
    #[error("path must not be empty")]
    EmptyPath,
    #[error("path {0:?} must be absolute")]
    RelativePath(Utf8PathBuf),
    #[error("path {0:?} contains a NUL byte")]
    NulByte(Utf8PathBuf),
}

/// Validates that a UTF-8 host or container path is non-empty, absolute and NUL-free.
pub fn validate_absolute_path(path: &Utf8Path) -> Result<(), ValidationError> {
    if path.as_str().is_empty() {
        return Err(ValidationError::EmptyPath);
    }
    if !path.is_absolute() {
        return Err(ValidationError::RelativePath(path.to_path_buf()));
    }
    if path.as_str().contains('\0') {
        return Err(ValidationError::NulByte(path.to_path_buf()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_rejects_empty() {
        assert_eq!(
            validate_absolute_path(Utf8Path::new("")),
            Err(ValidationError::EmptyPath)
        );
    }

    #[test]
    fn path_rejects_relative() {
        assert_eq!(
            validate_absolute_path(Utf8Path::new("dev/widget0")),
            Err(ValidationError::RelativePath(Utf8PathBuf::from(
                "dev/widget0"
            )))
        );
    }

    #[test]
    fn path_accepts_absolute() {
        assert!(validate_absolute_path(Utf8Path::new("/dev/widget0")).is_ok());
    }
    #[test]
    fn nul_path_is_rejected_after_absolute_path_check() {
        assert_eq!(
            validate_absolute_path(Utf8Path::new("/dev/a\0b")),
            Err(ValidationError::NulByte("/dev/a\0b".into()))
        );
        assert_eq!(
            validate_absolute_path(Utf8Path::new("a\0b")),
            Err(ValidationError::RelativePath("a\0b".into()))
        );
    }
    #[test]
    fn rejects_non_utf8_os_paths_at_the_conversion_boundary() {
        use std::os::unix::ffi::OsStrExt;
        let original = std::path::PathBuf::from(std::ffi::OsStr::from_bytes(b"/dev/widget-\xff"));
        assert_eq!(
            Utf8PathBuf::from_path_buf(original.clone()).unwrap_err(),
            original
        );
    }
}
