use std::ffi::OsString;
use std::path::{Path, PathBuf};

pub(crate) fn child_socket_path(public_socket_path: &Path) -> PathBuf {
    let mut value: OsString = public_socket_path.as_os_str().to_owned();
    value.push(".go");
    PathBuf::from(value)
}

pub(crate) fn callback_socket_path(public_socket_path: &Path) -> PathBuf {
    let mut value: OsString = public_socket_path.as_os_str().to_owned();
    value.push(".rs");
    PathBuf::from(value)
}

pub(crate) fn internal_socket_path(public_socket_path: &Path) -> PathBuf {
    let mut value: OsString = public_socket_path.as_os_str().to_owned();
    value.push(".in");
    PathBuf::from(value)
}

#[cfg(test)]
mod tests {
    use super::{callback_socket_path, child_socket_path, internal_socket_path};
    use std::path::Path;

    #[test]
    fn child_socket_never_aliases_public_socket() {
        let public = Path::new("/tmp/proletarius.sock");
        let child = child_socket_path(public);
        assert_eq!(child, Path::new("/tmp/proletarius.sock.go"));
        assert_ne!(child, public);
    }

    #[test]
    fn callback_socket_is_a_distinct_same_length_sibling() {
        let public = Path::new("/tmp/proletarius.sock");
        let callback = callback_socket_path(public);
        assert_eq!(callback, Path::new("/tmp/proletarius.sock.rs"));
        assert_ne!(callback, public);
        assert_ne!(callback, child_socket_path(public));
        assert_eq!(
            callback.as_os_str().len(),
            child_socket_path(public).as_os_str().len()
        );
    }

    #[test]
    fn internal_socket_is_distinct_and_keeps_the_unix_path_budget() {
        let public = Path::new("/tmp/proletarius.sock");
        let internal = internal_socket_path(public);
        assert_eq!(internal, Path::new("/tmp/proletarius.sock.in"));
        assert_ne!(internal, public);
        assert_ne!(internal, callback_socket_path(public));
        assert_ne!(internal, child_socket_path(public));
        assert_eq!(
            internal.as_os_str().len(),
            child_socket_path(public).as_os_str().len()
        );
    }
}
