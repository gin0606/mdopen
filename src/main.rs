use std::fmt;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("mdo: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), Error> {
    let mut args = std::env::args_os().skip(1);
    let (Some(input), None) = (args.next(), args.next()) else {
        return Err(Error::Usage);
    };

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
    let rendered = mdo::render(&markdown, &title, base_dir);
    for warning in &rendered.warnings {
        eprintln!("mdo: 警告: {warning}");
    }

    let destination = mdo::output_path(&source);
    write_private(&destination, &rendered.html).map_err(|error| Error::Write {
        path: destination.clone(),
        source: error,
    })?;

    opener::open(&destination).map_err(|source| Error::Open {
        path: destination,
        source,
    })
}

/// 変換結果を書き出す。ディレクトリとファイルは所有者だけが読める権限で作る。
///
/// 一時ディレクトリを他の利用者と共有する環境では、先に同名のディレクトリや symlink を
/// 置かれると書き込み先をすり替えられる。macOS の `TMPDIR` は利用者ごとに分かれている。
fn write_private(path: &Path, html: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        let mut directory = fs::DirBuilder::new();
        directory.recursive(true);
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(&mut directory, 0o700);
        directory.create(parent)?;
    }

    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);

    let mut file = options.open(path)?;
    // mode は新規作成にしか効かないので、緩いまま残っていた既存ファイルを締め直す。
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    file.write_all(html.as_bytes())
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
    Open {
        path: PathBuf,
        source: opener::OpenError,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Usage => write!(f, "usage: mdo <file.md>"),
            Error::Read { path, source } => write!(f, "{} を読めません: {source}", path.display()),
            Error::NotUtf8 { path } => write!(f, "{} が UTF-8 ではありません", path.display()),
            Error::Write { path, source } => {
                write!(f, "{} に書き出せません: {source}", path.display())
            }
            Error::Open { path, source } => {
                write!(f, "{} をブラウザで開けません: {source}", path.display())
            }
        }
    }
}
