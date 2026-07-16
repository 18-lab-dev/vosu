use std::fmt::Write as _;
use std::{fs, io};

const OVERLAYS_STATIC: &str = r#"<html>
  <head>
    <title>{PAGE_URL}</title>
    <link rel="icon" type="image/x-icon" href="/assets/favicon.ico" />
    <link rel="stylesheet" href="/assets/fonts/style.css" />

    <style>
      * {
        padding: 0;
        margin: 0;
        box-sizing: border-box;
        min-width: 0;
      }

      body {
        font-size: 2.5em;
        font-family: "Roboto", sans-serif;
        font-weight: 400;
        color: #c9abae;
        background: #110d0d;
      }

      ul {
        margin: 30px;
        list-style: decimal-leading-zero;
        display: grid;
        grid-template-columns: 25% 25% 25% 25%;
        padding: 0;
        content-visibility: auto;
        contain-intrinsic-size: 1000px;
      }

      li {
        margin-left: 2em;
      }

      a {
        color: #b5616b;
        text-decoration: underline;
        text-underline-offset: 6px;
        transition: 0.2s ease;
        transition-property: text-underline-offset, color;
        overflow-wrap: break-word;
      }

      a:hover {
        color: #c9abae;
        text-underline-offset: 4px;
      }
    </style>
  </head>

  <body>
    <ul id="overlays-list">
      {OVERLAYS_LIST}
    </ul>
  </body>
</html>"#;

#[cfg(target_os = "linux")]
fn directory_entries(directory: &fs::File) -> io::Result<Vec<(String, bool)>> {
    use std::os::fd::AsRawFd;

    fs::read_dir(format!("/proc/self/fd/{}", directory.as_raw_fd()))?
        .map(|entry| {
            let entry = entry?;
            let file_type = entry.file_type()?;
            Ok((
                entry.file_name().to_string_lossy().into_owned(),
                file_type.is_dir(),
            ))
        })
        .collect()
}

#[cfg(windows)]
fn directory_entries(directory: &fs::File) -> io::Result<Vec<(String, bool)>> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::{ERROR_NO_MORE_FILES, GetLastError, HANDLE};
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_DIRECTORY, FILE_ID_BOTH_DIR_INFO, FileIdBothDirectoryInfo,
        FileIdBothDirectoryRestartInfo, GetFileInformationByHandleEx,
    };

    let handle = directory.as_raw_handle() as HANDLE;
    let mut entries = Vec::new();
    let mut restart = true;
    loop {
        // u64 backing guarantees the alignment required by FILE_ID_BOTH_DIR_INFO.
        let mut storage = vec![0u64; 8192 / std::mem::size_of::<u64>()];
        let class = if restart {
            FileIdBothDirectoryRestartInfo
        } else {
            FileIdBothDirectoryInfo
        };
        let ok = unsafe {
            // SAFETY: storage is writable and remains alive for the whole query.
            GetFileInformationByHandleEx(
                handle,
                class,
                storage.as_mut_ptr().cast(),
                (storage.len() * std::mem::size_of::<u64>()) as u32,
            )
        };
        restart = false;
        if ok == 0 {
            let code = unsafe {
                // SAFETY: GetLastError has no preconditions.
                GetLastError()
            };
            if code == ERROR_NO_MORE_FILES {
                break;
            }
            return Err(io::Error::from_raw_os_error(code as i32));
        }

        let bytes = storage.len() * std::mem::size_of::<u64>();
        let mut offset = 0usize;
        loop {
            if offset + std::mem::size_of::<FILE_ID_BOTH_DIR_INFO>() > bytes {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "invalid directory information",
                ));
            }
            let info = unsafe {
                // SAFETY: offset is bounds checked and storage has suitable alignment.
                &*(storage.as_ptr().cast::<u8>().add(offset) as *const FILE_ID_BOTH_DIR_INFO)
            };
            let name_len = info.FileNameLength as usize;
            let name_offset = std::mem::offset_of!(FILE_ID_BOTH_DIR_INFO, FileName);
            if !name_len.is_multiple_of(2) || offset + name_offset + name_len > bytes {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "invalid directory entry name",
                ));
            }
            let name = unsafe {
                // SAFETY: the UTF-16 slice bounds were checked above.
                std::slice::from_raw_parts(
                    storage.as_ptr().cast::<u8>().add(offset + name_offset) as *const u16,
                    name_len / 2,
                )
            };
            let name = String::from_utf16_lossy(name);
            if name != "." && name != ".." {
                entries.push((name, info.FileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0));
            }
            if info.NextEntryOffset == 0 {
                break;
            }
            let next = info.NextEntryOffset as usize;
            if next == 0 || offset + next >= bytes {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "invalid directory entry offset",
                ));
            }
            offset += next;
        }
    }
    Ok(entries)
}

pub fn build_directory_index(directory: &fs::File, request_target: &str) -> io::Result<String> {
    let base = request_target.split(['?', '#']).next().unwrap_or("/");
    let base = if base.ends_with('/') {
        base.to_owned()
    } else {
        format!("{base}/")
    };
    let mut entries = directory_entries(directory)?;
    entries.sort_by(|left, right| left.0.cmp(&right.0));

    let mut items = String::new();
    for (name, is_directory) in entries {
        let slash = if is_directory { "/" } else { "" };
        let href = format!("{}{}{}", base, encode_path_segment(&name), slash);
        let _ = writeln!(
            items,
            "<li><a href=\"{}\">{}{}</a></li>",
            html_escape(&href),
            html_escape(&name),
            slash
        );
    }
    let page_url = html_escape(&format!("vosu - {base}"));
    Ok(OVERLAYS_STATIC
        .replace("{OVERLAYS_LIST}", items.trim_end())
        .replace("{PAGE_URL}", &page_url))
}

pub fn html_escape(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            _ => escaped.push(character),
        }
    }
    escaped
}

pub fn encode_path_segment(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            encoded.push(char::from(*byte));
        } else {
            let _ = write!(encoded, "%{byte:02X}");
        }
    }
    encoded
}
