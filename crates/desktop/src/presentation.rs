/// Simplify ordinary drive/UNC namespaces for display, keeping volume/device paths intact.
/// Callers retain the original UTF-16 path for filesystem and Shell actions.
pub fn display_path(path: &str) -> String {
    if let Some(rest) = path.strip_prefix(r"\\?\UNC\") {
        return format!(r"\\{rest}");
    }
    if let Some(rest) = path.strip_prefix(r"\\?\") {
        let bytes = rest.as_bytes();
        if bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && bytes[1..3] == *b":\\" {
            return rest.to_owned();
        }
    }
    path.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn drive_and_unc_display_preserve_unicode_and_device_path_meaning() {
        assert_eq!(
            display_path(r"\\?\D:\Files\İstanbul-Ω.txt"),
            r"D:\Files\İstanbul-Ω.txt"
        );
        assert_eq!(
            display_path(r"\\?\UNC\server\share\file.txt"),
            r"\\server\share\file.txt"
        );
        for path in [
            r"D:\Files\file.txt",
            r"\\server\share\file.txt",
            r"\\?\Volume{1234}\file.txt",
            r"\\.\device",
        ] {
            assert_eq!(display_path(path), path);
        }
    }
}
