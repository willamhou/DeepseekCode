use std::path::PathBuf;

/// Expand a leading `~` or `~/` to the user's home directory.
///
/// - `"~/x/y"` → `<HOME>/x/y` if `HOME` env is set
/// - `"~"` alone → `<HOME>` if set
/// - `"/abs/path"` → unchanged
/// - `"relative"` → unchanged
/// - `"~user/x"` → unchanged (we do not support `~username` syntax)
/// - `HOME` unset → input unchanged (caller treats as missing path)
#[allow(dead_code)] // used by upcoming user-level skills directory loading
pub fn expand_tilde(path: &str) -> PathBuf {
    if path == "~" {
        return std::env::var("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from(path));
    }
    if let Some(rest) = path.strip_prefix("~/") {
        return std::env::var("HOME")
            .map(|home| {
                let mut buf = PathBuf::from(home);
                buf.push(rest);
                buf
            })
            .unwrap_or_else(|_| PathBuf::from(path));
    }
    PathBuf::from(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_home<F: FnOnce()>(home: Option<&str>, f: F) {
        let saved = std::env::var("HOME").ok();
        match home {
            Some(h) => std::env::set_var("HOME", h),
            None => std::env::remove_var("HOME"),
        }
        f();
        match saved {
            Some(s) => std::env::set_var("HOME", s),
            None => std::env::remove_var("HOME"),
        }
    }

    #[test]
    fn expands_tilde_slash_prefix_to_home() {
        with_home(Some("/h/u"), || {
            assert_eq!(
                expand_tilde("~/.config/dscode/skills"),
                PathBuf::from("/h/u/.config/dscode/skills")
            );
        });
    }

    #[test]
    fn returns_absolute_path_unchanged() {
        with_home(Some("/h/u"), || {
            assert_eq!(expand_tilde("/abs/path"), PathBuf::from("/abs/path"));
        });
    }

    #[test]
    fn does_not_expand_tilde_username_syntax() {
        with_home(Some("/h/u"), || {
            assert_eq!(expand_tilde("~user/x"), PathBuf::from("~user/x"));
        });
    }

    #[test]
    fn returns_input_unchanged_when_home_unset() {
        with_home(None, || {
            assert_eq!(
                expand_tilde("~/.config/dscode/skills"),
                PathBuf::from("~/.config/dscode/skills")
            );
        });
    }
}
