//! Shared Switch data-root resolution. Resolve once from explicit configuration
//! or the application support directory; never fall back to another product.
use std::env;
use std::ffi::OsString;
use std::path::PathBuf;

pub(crate) fn resolve() -> Result<PathBuf, String> {
    resolve_values(env::var_os("SWITCH_DATA_DIR"), env::var_os("HOME"))
}

fn resolve_values(configured: Option<OsString>, home: Option<OsString>) -> Result<PathBuf, String> {
    if let Some(value) = configured {
        let path = PathBuf::from(value);
        if path.as_os_str().is_empty() || !path.is_absolute() {
            return Err("SWITCH_DATA_DIR must be a nonempty absolute path".into());
        }
        return Ok(path);
    }
    let home = home
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "HOME is unavailable for Switch data directory".to_owned())?;
    let home = PathBuf::from(home);
    if !home.is_absolute() {
        return Err("HOME must be absolute for Switch data directory".into());
    }
    Ok(home
        .join("Library")
        .join("Application Support")
        .join("Switch"))
}

#[cfg(test)]
mod tests {
    use super::resolve_values;
    use std::ffi::OsString;
    use std::path::Path;

    #[test]
    fn explicit_root_is_shared_and_absolute() {
        let path = resolve_values(Some(OsString::from("/isolated/switch")), None).unwrap();
        assert_eq!(path, Path::new("/isolated/switch"));
        assert!(resolve_values(Some(OsString::from("relative")), None).is_err());
        assert!(resolve_values(Some(OsString::new()), Some(OsString::from("/home/test"))).is_err());
    }

    #[test]
    fn default_is_product_specific_and_missing_home_fails_closed() {
        assert_eq!(
            resolve_values(None, Some(OsString::from("/home/test"))).unwrap(),
            Path::new("/home/test/Library/Application Support/Switch")
        );
        assert!(resolve_values(None, None).is_err());
        assert!(resolve_values(None, Some(OsString::from("relative"))).is_err());
    }
}
