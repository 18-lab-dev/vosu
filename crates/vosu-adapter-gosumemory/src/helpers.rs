pub(super) fn joined_path(folder: &str, background: &str) -> String {
    join_node_path(folder, background, std::path::MAIN_SEPARATOR)
}

pub(super) fn join_node_path(folder: &str, background: &str, separator: char) -> String {
    let mut joined = String::new();
    let mut root_length = 0;
    let mut saw_path = false;

    for path in [folder, background] {
        if path.is_empty() {
            continue;
        }
        let path = if separator == '\\' {
            path.replace('/', "\\")
        } else {
            path.to_owned()
        };

        if !saw_path {
            let leading = path
                .chars()
                .take_while(|character| *character == separator)
                .count();
            root_length = if separator == '\\' && leading >= 2 {
                2
            } else {
                usize::from(leading != 0)
            };
            saw_path = true;
        }
        if !joined.is_empty() && !joined.ends_with(separator) {
            joined.push(separator);
        }
        joined.push_str(path.trim_start_matches(separator));
    }

    for _ in 0..root_length {
        joined.insert(0, separator);
    }
    normalize_node_path(&joined, separator)
}

fn normalize_node_path(path: &str, separator: char) -> String {
    if path.is_empty() {
        return ".".to_owned();
    }

    let leading = path
        .chars()
        .take_while(|character| *character == separator)
        .count();
    let root_length = if separator == '\\' && leading >= 2 {
        2
    } else {
        usize::from(leading != 0)
    };
    let bytes = path.as_bytes();
    let rooted_drive = separator == '\\'
        && bytes.first().is_some_and(u8::is_ascii_alphabetic)
        && bytes.get(1) == Some(&b':')
        && bytes.get(2) == Some(&(separator as u8));
    let absolute = root_length != 0 || rooted_drive;
    let trailing = path.ends_with(separator);
    let protected_components = if separator == '\\' && root_length == 2 {
        2
    } else {
        usize::from(rooted_drive)
    };
    let mut components = Vec::new();
    for component in path.split(separator) {
        match component {
            "" | "." => {}
            ".." if components.len() > protected_components
                && components.last().is_some_and(|last| *last != "..") =>
            {
                components.pop();
            }
            ".." if !absolute => components.push(component),
            ".." => {}
            value => components.push(value),
        }
    }
    let is_unc_root = separator == '\\' && root_length == 2 && components.len() == 2;

    let mut result = components.join(&separator.to_string());
    for _ in 0..root_length {
        result.insert(0, separator);
    }
    if result.is_empty() {
        result.push(if absolute { separator } else { '.' });
    }
    let root = separator.to_string().repeat(root_length);
    if (trailing || is_unc_root) && result != root {
        result.push(separator);
    }
    result
}
