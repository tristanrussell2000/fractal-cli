/// Matches an ASCII value against a case-insensitive glob containing `*`
/// wildcards.
///
/// ABAP repository and package names are ASCII. Keeping this helper byte-based
/// makes the matching behavior explicit and avoids pulling a glob dependency
/// into the small set of patterns used by Fractal profiles.
pub fn glob_matches(pattern: &str, value: &str) -> bool {
    let pattern = pattern.to_ascii_uppercase();
    let value = value.to_ascii_uppercase();
    let pattern = pattern.as_bytes();
    let value = value.as_bytes();
    let mut pattern_index = 0;
    let mut value_index = 0;
    let mut star = None;
    let mut retry_value_index = 0;

    while value_index < value.len() {
        if pattern_index < pattern.len() && pattern[pattern_index] == value[value_index] {
            pattern_index += 1;
            value_index += 1;
        } else if pattern_index < pattern.len() && pattern[pattern_index] == b'*' {
            star = Some(pattern_index);
            pattern_index += 1;
            retry_value_index = value_index;
        } else if let Some(star_position) = star {
            pattern_index = star_position + 1;
            retry_value_index += 1;
            value_index = retry_value_index;
        } else {
            return false;
        }
    }

    while pattern_index < pattern.len() && pattern[pattern_index] == b'*' {
        pattern_index += 1;
    }
    pattern_index == pattern.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_case_insensitively() {
        assert!(glob_matches("Z*", "z_example"));
        assert!(glob_matches("/ACME/*", "/acme/example"));
    }

    #[test]
    fn supports_wildcards_inside_patterns() {
        assert!(glob_matches("Z*_TEST", "Z_LONG_TEST"));
        assert!(!glob_matches("Z*_TEST", "Z_LONG_DEMO"));
    }

    #[test]
    fn requires_the_complete_value_to_match() {
        assert!(glob_matches("Z_EXACT", "z_exact"));
        assert!(!glob_matches("Z_EXACT", "Z_EXACT_MORE"));
    }
}
