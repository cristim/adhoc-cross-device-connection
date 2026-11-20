//! Disk-backed bounded transfer decoding and atomic publication.
use anyhow::{bail, ensure, Context, Result};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Seek, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};
pub const MAX_TRANSFER: u64 = 32 * 1024 * 1024 * 1024;
pub const MAX_ENTRIES: usize = 65536;
pub const MIN_FREE_BYTES: u64 = 1024 * 1024 * 1024;
pub const DEFAULT_MAX_DISK_PERCENT: u8 = 30;

pub fn validate_max_disk_percent(percent: u8) -> Result<u8> {
    ensure!(
        (1..=90).contains(&percent),
        "max disk percent must be 1..=90"
    );
    Ok(percent)
}

pub fn allowed_transfer_bytes_for_free(free_bytes: u64, max_percent: u8) -> Result<u64> {
    let pct = validate_max_disk_percent(max_percent)?;
    let reserve_cap = free_bytes.saturating_sub(MIN_FREE_BYTES);
    ensure!(
        reserve_cap > 0,
        "insufficient free disk space (< 1 GiB free)"
    );
    let percent_cap = ((free_bytes as u128 * pct as u128) / 100) as u64;
    Ok(percent_cap.min(reserve_cap).min(MAX_TRANSFER))
}

pub fn available_disk_bytes(path: &Path) -> Result<u64> {
    use std::os::unix::ffi::OsStrExt;
    let c_path = std::ffi::CString::new(path.as_os_str().as_bytes())
        .context("invalid destination path for statvfs")?;
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::statvfs(c_path.as_ptr(), &mut stat) };
    ensure!(
        rc == 0,
        "statvfs failed: {}",
        std::io::Error::last_os_error()
    );
    let frsize = if stat.f_frsize > 0 {
        stat.f_frsize
    } else {
        stat.f_bsize
    };
    Ok((stat.f_bavail as u128)
        .saturating_mul(frsize as u128)
        .min(u64::MAX as u128) as u64)
}

pub fn max_allowed_bytes(dest: &Path, max_percent: u8) -> Result<u64> {
    let free = available_disk_bytes(dest)?;
    allowed_transfer_bytes_for_free(free, max_percent)
}
fn bounded_copy(mut source: impl Read, out: &mut impl Write, limit: u64) -> Result<u64> {
    let n = std::io::copy(&mut source.by_ref().take(limit + 1), out)?;
    ensure!(n <= limit, "transfer exceeds {limit} byte limit");
    Ok(n)
}
fn number(b: &[u8], radix: u32) -> Result<u64> {
    Ok(u64::from_str_radix(std::str::from_utf8(b)?, radix)?)
}
fn relative(name: &str) -> Result<PathBuf> {
    ensure!(
        !name.starts_with('/') && !name.contains('\\') && name.len() <= 4096,
        "unsafe archive path"
    );
    let mut result = PathBuf::new();
    for part in name.split('/') {
        if part == "." || part.is_empty() {
            continue;
        }
        ensure!(
            part != ".."
                && part.len() <= 255
                && !part.chars().any(|c| c.is_control()
                    || matches!(c,'\u{202a}'..='\u{202e}'|'\u{2066}'..='\u{2069}')),
            "unsafe archive component"
        );
        result.push(part);
    }
    ensure!(!result.as_os_str().is_empty(), "empty archive path");
    Ok(result)
}
fn decoded(
    mut file: File,
    ty: &str,
    token: super::cancel::Token,
    limit: u64,
) -> Result<tempfile::NamedTempFile> {
    let mut output = tempfile::NamedTempFile::new()?;
    let mut prefix = [0; 2];
    let count = file.read(&mut prefix)?;
    file.rewind()?;
    let mut file = super::cancel::Reader { inner: file, token };
    if count == 2 && prefix == [31, 139] {
        bounded_copy(
            flate2::read::GzDecoder::new(file),
            output.as_file_mut(),
            limit,
        )?;
    } else if ty == "application/x-dvzip" {
        let mut written = 0;
        loop {
            let mut header = [0; 4];
            let n = file.read(&mut header[..1])?;
            if n == 0 {
                break;
            }
            file.read_exact(&mut header[1..])?;
            let header = u32::from_be_bytes(header);
            let size = (header & 0x7fffffff) as u64;
            ensure!(size > 0, "empty DVZip block");
            let mut block = (&mut file).take(size);
            let n = if header & 0x80000000 != 0 {
                bounded_copy(&mut block, output.as_file_mut(), limit - written)?
            } else {
                bounded_copy(
                    flate2::read::ZlibDecoder::new(&mut block),
                    output.as_file_mut(),
                    limit - written,
                )?
            };
            // Consume the entire bounded block, including any decoder buffering.
            std::io::copy(&mut block, &mut std::io::sink())?;
            ensure!(block.limit() == 0, "truncated DVZip block");
            written += n;
        }
    } else {
        ensure!(ty == "application/x-cpio", "unsupported archive media type");
        bounded_copy(file, output.as_file_mut(), limit)?;
    }
    output.as_file_mut().rewind()?;
    Ok(output)
}
pub struct Saved {
    pub directory: PathBuf,
    pub files: usize,
    pub bytes: u64,
    pub clipboard: Vec<super::archive::Entry>,
}
#[cfg(test)]
pub fn receive(file: File, ty: &str, dest: &Path) -> Result<Saved> {
    receive_cancel(file, ty, dest, super::cancel::Token::default())
}
#[cfg(test)]
pub fn receive_cancel(
    file: File,
    ty: &str,
    dest: &Path,
    token: super::cancel::Token,
) -> Result<Saved> {
    receive_cancel_with_limit(file, ty, dest, token, MAX_TRANSFER)
}

pub fn receive_cancel_with_limit(
    file: File,
    ty: &str,
    dest: &Path,
    token: super::cancel::Token,
    limit: u64,
) -> Result<Saved> {
    let decoded = decoded(file, ty, token.clone(), limit)?;
    let mut input = std::io::BufReader::new(super::cancel::Reader {
        inner: decoded.as_file(),
        token: token.clone(),
    });
    let staging = tempfile::Builder::new()
        .prefix(".ac-dc-incoming-")
        .tempdir_in(dest)?;
    let mut offset = 0u64;
    let mut total = 0;
    let mut files = 0;
    let mut clipboard = Vec::new();
    let mut clipboard_bytes = 0usize;
    let mut trailer = false;
    for _ in 0..MAX_ENTRIES {
        let mut magic = [0; 6];
        input
            .read_exact(&mut magic)
            .context("CPIO missing trailer")?;
        let (mode, size, namesize, align, hlen) = match &magic {
            b"070707" => {
                let mut h = [0; 70];
                input.read_exact(&mut h)?;
                (
                    number(&h[12..18], 8)?,
                    number(&h[59..70], 8)?,
                    number(&h[53..59], 8)?,
                    1,
                    76,
                )
            }
            b"070701" => {
                let mut h = [0; 104];
                input.read_exact(&mut h)?;
                (
                    number(&h[8..16], 16)?,
                    number(&h[48..56], 16)?,
                    number(&h[88..96], 16)?,
                    4,
                    110,
                )
            }
            _ => bail!("unsupported CPIO format"),
        };
        ensure!((1..=4096).contains(&namesize), "invalid CPIO name length");
        let mut name = vec![0; namesize as usize];
        input.read_exact(&mut name)?;
        ensure!(name.pop() == Some(0), "CPIO name not terminated");
        let name = std::str::from_utf8(&name)?;
        offset += hlen + namesize;
        pad(&mut input, &mut offset, align)?;
        if name == "TRAILER!!!" {
            ensure!(size == 0, "invalid CPIO trailer");
            trailer = true;
            break;
        }
        ensure!(size <= limit - total, "archive exceeds limit");
        total += size;
        if name == "." || name == "./" {
            ensure!(
                mode & 0o170000 == 0o040000 && size == 0,
                "invalid root entry"
            );
            continue;
        }
        let path = relative(name)?;
        let target = staging.path().join(&path);
        match mode & 0o170000 {
            0o040000 => {
                ensure!(size == 0, "directory has payload");
                std::fs::create_dir_all(&target)?;
                std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o700))?;
            }
            0o100000 => {
                if path
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("._")
                {
                    let n = std::io::copy(&mut input.by_ref().take(size), &mut std::io::sink())?;
                    ensure!(n == size, "truncated metadata");
                } else {
                    std::fs::create_dir_all(target.parent().unwrap())?;
                    let mut out = OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .mode(0o600)
                        .open(&target)?;
                    let n = std::io::copy(&mut input.by_ref().take(size), &mut out)?;
                    ensure!(n == size, "truncated CPIO file");
                    out.sync_all()?;
                    files += 1;
                    // Clipboard copies stay small even when a transfer is many GiB.
                    if size <= 8 * 1024 * 1024
                        && clipboard_bytes + size as usize <= 8 * 1024 * 1024
                        && matches!(
                            path.extension().and_then(|s| s.to_str()),
                            Some("txt" | "png" | "jpg" | "jpeg" | "gif" | "webp")
                        )
                    {
                        let bytes = std::fs::read(&target)?;
                        clipboard_bytes += bytes.len();
                        clipboard.push(super::archive::Entry {
                            name: path.to_string_lossy().into(),
                            bytes,
                        });
                    }
                }
            }
            _ => bail!("archive links and special files are not supported"),
        }
        offset += size;
        pad(&mut input, &mut offset, align)?;
    }
    ensure!(trailer, "too many archive entries or missing trailer");
    ensure!(files > 0, "archive contains no regular files");
    token.check()?;
    // Publish top-level items directly into the selected receive directory, as
    // Airdrop-compatible does on macOS.  Never replace an existing item: Finder-style
    // collisions are numbered before the extension ("photo 2.jpg").
    use std::os::unix::ffi::OsStrExt;
    for entry in std::fs::read_dir(staging.path())? {
        let entry = entry?;
        let source = entry.path();
        let name = entry.file_name();
        let path = Path::new(&name);
        let stem = path.file_stem().unwrap_or(path.as_os_str());
        let extension = path.extension();
        let mut number = 1u64;
        loop {
            let candidate = if number == 1 {
                dest.join(&name)
            } else {
                let mut numbered = stem.to_os_string();
                numbered.push(format!(" {number}"));
                if let Some(extension) = extension {
                    numbered.push(".");
                    numbered.push(extension);
                }
                dest.join(numbered)
            };
            let old = std::ffi::CString::new(source.as_os_str().as_bytes())?;
            let new = std::ffi::CString::new(candidate.as_os_str().as_bytes())?;
            let rc = unsafe {
                libc::renameat2(
                    libc::AT_FDCWD,
                    old.as_ptr(),
                    libc::AT_FDCWD,
                    new.as_ptr(),
                    libc::RENAME_NOREPLACE,
                )
            };
            if rc == 0 {
                break;
            }
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::AlreadyExists {
                number = number
                    .checked_add(1)
                    .context("too many filename collisions")?;
                continue;
            }
            return Err(error).context("publish transfer");
        }
    }
    File::open(dest)?.sync_all()?;
    Ok(Saved {
        directory: dest.to_path_buf(),
        files,
        bytes: total,
        clipboard,
    })
}
fn pad(input: &mut impl Read, offset: &mut u64, align: u64) -> Result<()> {
    let n = (align - (*offset % align)) % align;
    let mut b = [0; 3];
    input.read_exact(&mut b[..n as usize])?;
    *offset += n;
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unsafe_paths_rejected() {
        for p in [
            "/absolute",
            "../escape",
            "x/../../y",
            "a\\b",
            "\u{202e}hidden",
        ] {
            assert!(relative(p).is_err(), "{p}");
        }
        assert_eq!(
            relative("./folder/file.txt").unwrap(),
            PathBuf::from("folder/file.txt")
        );
    }

    #[test]
    fn disk_quota_enforces_percent_and_one_gib_reserve() {
        assert!(validate_max_disk_percent(0).is_err());
        assert!(validate_max_disk_percent(91).is_err());
        assert_eq!(validate_max_disk_percent(30).unwrap(), 30);

        // < 1 GiB free is always rejected without underflow.
        assert!(allowed_transfer_bytes_for_free(512 * 1024 * 1024, 30).is_err());
        assert!(allowed_transfer_bytes_for_free(MIN_FREE_BYTES, 30).is_err());

        // 10 GiB free at 30% -> 3 GiB cap (reserve cap is 9 GiB).
        let ten_gib = 10 * MIN_FREE_BYTES;
        assert_eq!(
            allowed_transfer_bytes_for_free(ten_gib, 30).unwrap(),
            3 * MIN_FREE_BYTES
        );

        // 1.1 GiB free at 50% -> reserve cap (0.1 GiB) is tighter than 50% (0.55 GiB).
        let just_over = MIN_FREE_BYTES + 100 * 1024 * 1024;
        assert_eq!(
            allowed_transfer_bytes_for_free(just_over, 50).unwrap(),
            100 * 1024 * 1024
        );
    }

    #[test]
    fn quota_rejection_names_the_quota_that_applied() {
        let error = bounded_copy(&[0u8; 10][..], &mut Vec::new(), 5).unwrap_err();
        assert!(error.to_string().contains("5 byte limit"), "{error}");
    }
}
