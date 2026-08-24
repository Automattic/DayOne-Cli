use std::sync::OnceLock;

use time::OffsetDateTime;

#[cfg(unix)]
use std::ffi::CString;
#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;

#[cfg(unix)]
pub fn set_owner_only_permissions(path: &std::path::Path, mode: u32) -> std::io::Result<()> {
    if std::fs::symlink_metadata(path)?.file_type().is_symlink() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "refusing to change permissions through a symbolic link",
        ));
    }
    let path = CString::new(path.as_os_str().as_bytes()).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "Day One path contains a null byte",
        )
    })?;
    let native_mode = libc::mode_t::try_from(mode).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "invalid Unix permission mode",
        )
    })?;
    // SAFETY: `path` is a valid C string and `native_mode` contains only permission bits.
    let result = unsafe {
        libc::fchmodat(
            libc::AT_FDCWD,
            path.as_ptr(),
            native_mode,
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if result == 0 {
        return Ok(());
    }
    let error = std::io::Error::last_os_error();
    Err({
        std::io::Error::new(
            error.kind(),
            format!(
                "{error}; expected mode {mode:04o}. Inspect ownership and permissions for this Day One path, then retry. If it should belong to your account, an administrator can change its owner before you rerun the command"
            ),
        )
    })
}

#[cfg(unix)]
pub fn current_uid_string() -> std::io::Result<String> {
    let output = std::process::Command::new("id").arg("-u").output()?;
    if !output.status.success() {
        return Err(std::io::Error::other(format!(
            "id -u failed with status {}",
            output.status
        )));
    }
    let uid = String::from_utf8(output.stdout)
        .map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "id -u returned non-UTF-8 output",
            )
        })?
        .trim()
        .to_owned();
    if uid.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "id -u returned an empty UID",
        ));
    }
    Ok(uid)
}

pub fn now_rfc3339_utc() -> String {
    const RFC3339_MILLIS: &str =
        "[year]-[month]-[day]T[hour]:[minute]:[second].[subsecond digits:3]Z";
    static RFC3339_MILLIS_FORMAT: OnceLock<Vec<time::format_description::FormatItem<'static>>> =
        OnceLock::new();
    let format = RFC3339_MILLIS_FORMAT.get_or_init(|| {
        time::format_description::parse(RFC3339_MILLIS)
            .expect("RFC3339 millis format description must be valid")
    });
    OffsetDateTime::now_utc()
        .format(format)
        .expect("RFC3339 millis timestamp formatting should not fail")
}

/// Current Unix time in milliseconds. Saturates to 0 before the epoch, and to
/// `i64::MAX` if the millisecond count ever exceeds `i64` (not reachable for
/// real clocks, but kept total so callers never panic).
pub fn now_epoch_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

/// Current Unix time in milliseconds, formatted as a decimal string. Used for
/// Tracks `_ts` / `_rt` parameters.
pub fn now_epoch_ms_string() -> String {
    now_epoch_ms().to_string()
}

pub fn encode_path_segment(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for b in input.bytes() {
        let is_unreserved = matches!(
            b,
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~'
        );
        if is_unreserved {
            out.push(char::from(b));
        } else {
            out.push('%');
            out.push(hex_char((b >> 4) & 0x0F));
            out.push(hex_char(b & 0x0F));
        }
    }
    out
}

pub fn normalize_yyyy_mm_dd(input: &str) -> Option<String> {
    let date = input.trim();
    if date.len() != 10 {
        return None;
    }
    let bytes = date.as_bytes();
    if bytes[4] != b'-' || bytes[7] != b'-' {
        return None;
    }
    let valid = [0usize, 1, 2, 3, 5, 6, 8, 9]
        .iter()
        .all(|idx| bytes[*idx].is_ascii_digit());
    if !valid {
        return None;
    }
    Some(date.to_owned())
}

pub fn normalize_entry_id(input: &str) -> String {
    let trimmed = input.trim();
    if trimmed.len() == 32 && trimmed.chars().all(|c| c.is_ascii_hexdigit()) {
        return trimmed.to_ascii_uppercase();
    }
    trimmed.to_owned()
}

fn hex_char(n: u8) -> char {
    match n {
        0..=9 => char::from(b'0' + n),
        10..=15 => char::from(b'A' + (n - 10)),
        _ => unreachable!("nibble out of range"),
    }
}
