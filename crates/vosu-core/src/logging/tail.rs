//! Snapshot and follow the daemon's files without subscribing to its process.

use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use tokio_util::sync::CancellationToken;

const POLL_INTERVAL: Duration = Duration::from_millis(200);
const CHUNK: usize = 8192;
const ANCHOR: usize = 64;

/// The log directory beneath the resolved application data root.
pub fn logs_dir(paths: &vosu_sys::platform::Paths) -> PathBuf {
    paths.data().join("logs")
}

/// Resolves the native log directory of the daemon built for `channel`, using
/// the shared path policy.
pub fn system_logs_dir(channel: vosu_sys::platform::BuildChannel) -> io::Result<PathBuf> {
    Ok(logs_dir(&vosu_sys::platform::Paths::system(channel)?))
}

/// Prints the last `lines` lines across retained daemon logs to stdout.
///
/// With `follow`, polls until `cancellation` is cancelled; the caller owns signal
/// handling. A missing directory is empty, and follow waits for its creation.
/// Rotation preserves unread bytes in the old file before reading new files;
/// truncation restarts at byte zero. Output is unchanged bytes, including partial
/// final lines, and is flushed after every poll. Filesystem and stdout operations
/// run on Tokio's blocking pool, not on a runtime worker.
///
/// Like polling `tail`, truncate-and-regrow entirely between polls cannot be
/// distinguished from append when the bytes preceding the read offset match.
pub async fn print_logs(
    logs_dir: &Path,
    lines: usize,
    follow: bool,
    cancellation: CancellationToken,
) -> io::Result<()> {
    run(
        logs_dir.to_owned(),
        lines,
        follow,
        cancellation,
        io::stdout(),
    )
    .await
}

async fn run<W: Write + Send + 'static>(
    directory: PathBuf,
    lines: usize,
    follow: bool,
    cancellation: CancellationToken,
    mut output: W,
) -> io::Result<()> {
    if cancellation.is_cancelled() {
        return Ok(());
    }
    let mut state = tokio::task::spawn_blocking(move || {
        let mut state = Tail::new(directory)?;
        state.snapshot(lines, &mut output)?;
        Ok::<_, io::Error>((state, output))
    })
    .await
    .map_err(io::Error::other)??;
    if !follow {
        return Ok(());
    }
    loop {
        tokio::select! {
            biased;
            () = cancellation.cancelled() => return Ok(()),
            () = tokio::time::sleep(POLL_INTERVAL) => {}
        }
        state = tokio::task::spawn_blocking(move || {
            state.0.poll(&mut state.1)?;
            Ok::<_, io::Error>(state)
        })
        .await
        .map_err(io::Error::other)??;
    }
}

#[derive(Eq, PartialEq)]
struct Identity {
    device: u64,
    inode: u64,
}

impl Identity {
    fn of(file: &File) -> io::Result<Self> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let metadata = file.metadata()?;
            Ok(Self {
                device: metadata.dev(),
                inode: metadata.ino(),
            })
        }
        #[cfg(windows)]
        {
            let (device, inode) = vosu_sys::platform::file_identity(file)?;
            Ok(Self { device, inode })
        }
    }
}

struct Cursor {
    identity: Identity,
    file: File,
    offset: u64,
    anchor: Vec<u8>,
}

impl Cursor {
    fn open(path: &Path) -> io::Result<Self> {
        let file = File::open(path)?;
        Ok(Self {
            identity: Identity::of(&file)?,
            file,
            offset: 0,
            anchor: Vec::new(),
        })
    }

    fn remember(&mut self) -> io::Result<()> {
        let length = self.offset.min(ANCHOR as u64) as usize;
        self.anchor.resize(length, 0);
        self.file
            .seek(SeekFrom::Start(self.offset - length as u64))?;
        if let Err(error) = self.file.read_exact(&mut self.anchor) {
            if error.kind() != io::ErrorKind::UnexpectedEof {
                return Err(error);
            }
            // A concurrent truncate raced the copy or snapshot. Retry from zero.
            self.offset = 0;
            self.anchor.clear();
        }
        Ok(())
    }

    fn drain(&mut self, output: &mut impl Write) -> io::Result<()> {
        let end = self.file.metadata()?.len();
        let mut changed = end < self.offset;
        if !changed && !self.anchor.is_empty() {
            let mut actual = [0; ANCHOR];
            self.file
                .seek(SeekFrom::Start(self.offset - self.anchor.len() as u64))?;
            let actual = &mut actual[..self.anchor.len()];
            match self.file.read_exact(actual) {
                Ok(()) => changed = actual != self.anchor.as_slice(),
                Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => changed = true,
                Err(error) => return Err(error),
            }
        }
        if changed {
            self.offset = 0;
        }
        self.file.seek(SeekFrom::Start(self.offset))?;
        // Bound each poll to the observed size even if the daemon keeps writing.
        self.offset += io::copy(
            &mut (&mut self.file).take(end.saturating_sub(self.offset)),
            output,
        )?;
        self.remember()
    }
}

struct Tail {
    directory: PathBuf,
    files: Vec<Cursor>,
}

impl Tail {
    fn new(directory: PathBuf) -> io::Result<Self> {
        let mut state = Self {
            directory,
            files: Vec::new(),
        };
        for path in log_files(&state.directory)? {
            match Cursor::open(&path) {
                Ok(cursor) => state.files.push(cursor),
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
        Ok(state)
    }

    fn snapshot(&mut self, mut remaining: usize, output: &mut impl Write) -> io::Result<()> {
        let mut ranges = Vec::new();
        for cursor in self.files.iter_mut().rev() {
            let end = cursor.file.metadata()?.len();
            cursor.offset = end;
            let (start, count) = tail_start(&mut cursor.file, end, remaining)?;
            remaining = remaining.saturating_sub(count);
            ranges.push((start, end));
        }
        for (cursor, (start, end)) in self.files.iter_mut().zip(ranges.into_iter().rev()) {
            cursor.file.seek(SeekFrom::Start(start))?;
            io::copy(&mut (&mut cursor.file).take(end - start), output)?;
            cursor.remember()?;
        }
        output.flush()
    }

    fn poll(&mut self, output: &mut impl Write) -> io::Result<()> {
        let mut discovered = Self::new(self.directory.clone())?.files;
        // Drain held handles first: an archive may have been renamed or removed.
        for cursor in &mut self.files {
            cursor.drain(output)?;
        }
        self.files
            .retain(|cursor| discovered.iter().any(|new| new.identity == cursor.identity));
        for mut cursor in discovered.drain(..) {
            if !self.files.iter().any(|old| old.identity == cursor.identity) {
                cursor.drain(output)?;
                self.files.push(cursor);
            }
        }
        output.flush()
    }
}

/// Locate the tail by scanning backwards, without loading an entire log.
fn tail_start(file: &mut File, end: u64, lines: usize) -> io::Result<(u64, usize)> {
    if lines == 0 || end == 0 {
        return Ok((end, 0));
    }
    let mut position = end;
    let mut count = 0;
    let mut buffer = [0; CHUNK];
    while position > 0 {
        let length = position.min(CHUNK as u64) as usize;
        position -= length as u64;
        file.seek(SeekFrom::Start(position))?;
        file.read_exact(&mut buffer[..length])?;
        for index in (0..length).rev() {
            let absolute = position + index as u64;
            if buffer[index] == b'\n' && absolute != end - 1 {
                count += 1;
                if count == lines {
                    return Ok((absolute + 1, count));
                }
            }
        }
    }
    Ok((0, count + 1))
}

fn log_files(directory: &Path) -> io::Result<Vec<PathBuf>> {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let mut files = Vec::new();
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let key = if name == "latest.log" {
            (true, String::new(), 0)
        } else {
            let Some(stem) = name.strip_suffix(".log") else {
                continue;
            };
            let Some(date) = stem.get(..19) else { continue };
            if chrono::NaiveDateTime::parse_from_str(date, "%Y-%m-%d %H-%M-%S").is_err() {
                continue;
            }
            let suffix = match &stem[19..] {
                "" => 0,
                rest => match rest
                    .strip_prefix('-')
                    .and_then(|value| value.parse::<u64>().ok())
                {
                    Some(suffix) => suffix,
                    None => continue,
                },
            };
            (false, date.to_owned(), suffix)
        };
        match entry.file_type() {
            Ok(kind) if kind.is_file() => files.push((key, entry.path())),
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    files.sort_unstable_by(|left, right| left.0.cmp(&right.0));
    Ok(files.into_iter().map(|(_, path)| path).collect())
}

#[cfg(test)]
mod tests;
