/// The severity of one diagnostic message returned by an ADT operation.
///
/// SAP reports severity as a short code on both `checkMessage` elements and
/// activation `msg` elements, and both use the same encoding: `E` or `A` for
/// errors and aborts, `W` for warnings, and anything else — `I`, `S`, a
/// spelled-out word such as `warning`, or no code at all — for informational
/// text. The two workflows read the code from different attributes, but they
/// classify it identically, so the classification lives here once.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdtMessageSeverity {
    Error,
    Warning,
    Info,
}

impl AdtMessageSeverity {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warning => "warning",
            Self::Info => "info",
        }
    }

    /// Classifies a raw SAP severity code such as `E`, `W`, or `warning`.
    ///
    /// Unrecognized and empty codes are informational rather than an error, so
    /// an unexpected SAP code can never silently promote a message into the
    /// error count that blocks activation.
    #[must_use]
    pub fn from_sap_code(code: &str) -> Self {
        let code = code.to_ascii_uppercase();
        if code.starts_with('E') || code.starts_with('A') {
            Self::Error
        } else if code.starts_with('W') {
            Self::Warning
        } else {
            Self::Info
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_the_sap_severity_codes_used_by_check_and_activation() {
        for code in ["E", "e", "ERROR", "A", "abort"] {
            assert_eq!(
                AdtMessageSeverity::from_sap_code(code),
                AdtMessageSeverity::Error
            );
        }
        for code in ["W", "w", "warning"] {
            assert_eq!(
                AdtMessageSeverity::from_sap_code(code),
                AdtMessageSeverity::Warning
            );
        }
        for code in ["I", "S", "", "unexpected"] {
            assert_eq!(
                AdtMessageSeverity::from_sap_code(code),
                AdtMessageSeverity::Info
            );
        }
    }

    #[test]
    fn renders_stable_lowercase_names_for_command_output() {
        assert_eq!(AdtMessageSeverity::Error.as_str(), "error");
        assert_eq!(AdtMessageSeverity::Warning.as_str(), "warning");
        assert_eq!(AdtMessageSeverity::Info.as_str(), "info");
    }
}
