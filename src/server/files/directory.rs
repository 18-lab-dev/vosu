use std::fmt::Write as _;
use std::{fs, io};

const OVERLAYS_STATIC: &str = r##"<!doctype html>
<html lang="en">
  <head>
    <meta charset="utf-8">
    <meta name="viewport" content="width=device-width, initial-scale=1">
    <meta name="color-scheme" content="dark">
    <meta name="theme-color" content="#100E18">
    <title>{PAGE_URL}</title>
    <link rel="icon" href="/favicon.ico" sizes="any">
    <style>
      :root {
        color-scheme: dark;
        font-family: ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
        color: #F9F7FF;
        background: #100E18;
      }

      * {
        box-sizing: border-box;
      }

      body {
        width: min(100% - 2rem, 72rem);
        min-width: 18rem;
        min-height: 100vh;
        margin: 0 auto;
        background:
          radial-gradient(circle at top right, rgba(122, 225, 248, 0.12), transparent 26rem),
          #100E18;
      }

      header {
        padding: 2.5rem 0 1.25rem;
        border-bottom: 1px solid rgba(249, 247, 255, 0.16);
      }

      .eyebrow {
        margin: 0 0 0.4rem;
        color: #F45CAD;
        font-size: 0.75rem;
        font-weight: 800;
        letter-spacing: 0.12em;
        text-transform: uppercase;
      }

      h1 {
        margin: 0;
        overflow-wrap: anywhere;
        font-size: clamp(1.6rem, 5vw, 2.8rem);
      }

      #overlays-list {
        display: grid;
        grid-template-columns: repeat(auto-fit, minmax(min(100%, 18rem), 1fr));
        gap: 0.65rem;
        margin: 0;
        padding: 1.5rem 0 3rem;
        list-style: none;
      }

      #overlays-list li {
        min-width: 0;
      }

      #overlays-list a {
        display: block;
        min-height: 3rem;
        padding: 0.8rem 0.9rem;
        overflow-wrap: anywhere;
        color: #7AE1F8;
        font-weight: 650;
        text-decoration: none;
        background: #181423;
        border: 1px solid rgba(249, 247, 255, 0.16);
        border-radius: 0.65rem;
      }

      #overlays-list a:hover {
        color: #F9F7FF;
        border-color: #F45CAD;
      }

      #overlays-list a:focus-visible {
        outline: 0.2rem solid #7AE1F8;
        outline-offset: 0.2rem;
      }
    </style>
  </head>
  <body>
    <header>
      <p class="eyebrow">Vosu file index</p>
      <h1>{PAGE_URL}</h1>
    </header>
    <ul id="overlays-list">
      {OVERLAYS_LIST}
    </ul>
  </body>
</html>"##;

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
