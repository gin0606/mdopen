use std::fmt;
use std::fs;
use std::io::Write;
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::{
    DirBuilderExt as _, MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _,
};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicU64, Ordering};

const USAGE: &str = "usage: mdhtml <file.md>";

static NEXT_TEMPORARY_FILE: AtomicU64 = AtomicU64::new(0);

const HELP: &str = "\
Markdown を HTML 1 枚に変換して、その置き場のパスを返す。

usage: mdhtml <file.md>

options:
  -h, --help     使い方を表示する
  -V, --version  版を表示する";

/// 版は git tag が唯一の真実なので、justfile がビルド時に渡す。cargo から直接
/// 組んだものはどの tag にも対応しないので、版を名乗らせない。
const VERSION: &str = match option_env!("MDO_VERSION") {
    Some(version) => version,
    None => "dev",
};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("mdhtml: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), Error> {
    let mut args = std::env::args_os().skip(1);
    let (Some(input), None) = (args.next(), args.next()) else {
        return Err(Error::Usage);
    };

    // 引数はファイル 1 つだけなので、`--` 区切りは持たない。`-foo.md` は `./-foo.md` と書く。
    if let Some(flag) = input.to_str().filter(|arg| arg.starts_with('-')) {
        return match flag {
            "-h" | "--help" => {
                println!("{HELP}");
                Ok(())
            }
            "-V" | "--version" => {
                println!("mdhtml {VERSION}");
                Ok(())
            }
            _ => Err(Error::Usage),
        };
    }

    let source = Path::new(&input)
        .canonicalize()
        .map_err(|error| Error::Read {
            path: PathBuf::from(&input),
            source: error,
        })?;

    let bytes = fs::read(&source).map_err(|error| Error::Read {
        path: source.clone(),
        source: error,
    })?;
    let markdown = String::from_utf8(bytes).map_err(|_| Error::NotUtf8 {
        path: source.clone(),
    })?;

    let title = source
        .file_name()
        .unwrap_or(source.as_os_str())
        .to_string_lossy();
    let base_dir = source.parent().unwrap_or(Path::new("."));
    let rendered = mdopen::render(&markdown, &title, base_dir);
    for warning in &rendered.warnings {
        eprintln!("mdhtml: 警告: {warning}");
    }

    let destination = mdopen::output_path(&source);
    write_private(&destination, &rendered.html).map_err(|error| Error::Write {
        path: destination.clone(),
        source: error,
    })?;

    write_path(&mut std::io::stdout().lock(), &destination)
        .map_err(|source| Error::Print { source })
}

/// 変換結果の置き場を 1 行返す。開くのは受け取った側の仕事。
///
/// 受け取った側はこのパスをそのまま `open` に渡す。`display()` は UTF-8 でないバイトを
/// 置換文字に潰すので、書き出した先とは別のパスを名乗ってしまう。
fn write_path(out: &mut impl Write, path: &Path) -> std::io::Result<()> {
    out.write_all(path.as_os_str().as_bytes())?;
    out.write_all(b"\n")
}

/// 変換結果を所有者限定の一時ファイルに書き、完成後に公開パスへ置き換える。
fn write_private(path: &Path, html: &str) -> std::io::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "出力先に親ディレクトリがありません",
        )
    })?;
    ensure_private_directory(parent)?;

    let (temporary, mut file) = create_temporary_file(path)?;
    if let Err(error) = file.write_all(html.as_bytes()) {
        drop(file);
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }
    drop(file);

    if let Err(error) = fs::rename(&temporary, path) {
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }

    Ok(())
}

/// 出力先は予測可能なため、共有一時領域に先回りで作られたディレクトリを使わない。
fn ensure_private_directory(path: &Path) -> std::io::Result<()> {
    let mut directory = fs::DirBuilder::new();
    directory.recursive(true).mode(0o700);
    directory.create(path)?;

    let metadata = fs::symlink_metadata(path)?;
    // SAFETY: geteuid has no preconditions and does not dereference pointers.
    let effective_uid = unsafe { libc::geteuid() };
    if !metadata.file_type().is_dir()
        || metadata.uid() != effective_uid
        || metadata.permissions().mode() & 0o077 != 0
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!(
                "{} が所有者限定のディレクトリではありません",
                path.display()
            ),
        ));
    }

    Ok(())
}

fn create_temporary_file(path: &Path) -> std::io::Result<(PathBuf, fs::File)> {
    let parent = path.parent().expect("呼び出し元で確認済み");
    let name = path.file_name().unwrap_or_default().to_string_lossy();

    loop {
        let serial = NEXT_TEMPORARY_FILE.fetch_add(1, Ordering::Relaxed);
        let temporary = parent.join(format!(".{name}.{}.{serial}.tmp", std::process::id()));

        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true).mode(0o600);
        match options.open(&temporary) {
            Ok(file) => return Ok((temporary, file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
}

enum Error {
    Usage,
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    NotUtf8 {
        path: PathBuf,
    },
    Write {
        path: PathBuf,
        source: std::io::Error,
    },
    Print {
        source: std::io::Error,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Usage => write!(f, "{USAGE}"),
            Error::Read { path, source } => write!(f, "{} を読めません: {source}", path.display()),
            Error::NotUtf8 { path } => write!(f, "{} が UTF-8 ではありません", path.display()),
            Error::Write { path, source } => {
                write!(f, "{} に書き出せません: {source}", path.display())
            }
            Error::Print { source } => {
                write!(f, "変換結果のパスを出力できません: {source}")
            }
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mdhtml-test-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&dir)
            .unwrap();
        dir
    }

    fn mode_of(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt as _;
        fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn writing_does_not_follow_a_symlink() {
        let dir = scratch("symlink");
        let victim = dir.join("victim.txt");
        let destination = dir.join("out.html");
        fs::write(&victim, "victim").unwrap();
        std::os::unix::fs::symlink(&victim, &destination).unwrap();

        write_private(&destination, "<html>").unwrap();

        assert_eq!(fs::read_to_string(&victim).unwrap(), "victim");
        assert_eq!(fs::read_to_string(&destination).unwrap(), "<html>");
        assert_eq!(mode_of(&destination), 0o600);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn writing_replaces_the_previous_output() {
        let dir = scratch("replace");
        let destination = dir.join("out.html");

        write_private(&destination, "old").unwrap();
        write_private(&destination, "new").unwrap();

        assert_eq!(fs::read_to_string(&destination).unwrap(), "new");
        assert_eq!(mode_of(&destination), 0o600);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn writing_rejects_a_symlinked_directory() {
        let dir = scratch("directory-symlink");
        let actual = dir.join("actual");
        fs::DirBuilder::new().mode(0o700).create(&actual).unwrap();
        let linked = dir.join("linked");
        std::os::unix::fs::symlink(&actual, &linked).unwrap();

        let error = write_private(&linked.join("out.html"), "<html>").unwrap_err();

        assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
        assert!(!actual.join("out.html").exists());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn writing_rejects_a_directory_accessible_to_other_users() {
        let dir = scratch("permissive-directory");
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();

        let error = write_private(&dir.join("out.html"), "<html>").unwrap_err();

        assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn concurrent_writes_publish_only_complete_pages() {
        let dir = scratch("concurrent");
        let destination = dir.join("out.html");
        let pages: Vec<_> = (0..32).map(|i| format!("page-{i}").repeat(1_000)).collect();

        std::thread::scope(|scope| {
            for page in &pages {
                scope.spawn(|| write_private(&destination, page).unwrap());
            }
        });

        let actual = fs::read_to_string(&destination).unwrap();
        assert!(pages.contains(&actual));
        assert_eq!(mode_of(&destination), 0o600);
        fs::remove_dir_all(&dir).unwrap();
    }
}
