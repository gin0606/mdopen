use std::fmt;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

const USAGE: &str = "usage: mdopen <file.md>";

const HELP: &str = "\
Markdown を HTML 1 枚に変換して、既定のブラウザで開く。

usage: mdopen <file.md>

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
            eprintln!("mdopen: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), Error> {
    let mut args = std::env::args_os().skip(1);
    let (Some(input), None) = (args.next(), args.next()) else {
        return Err(Error::Usage);
    };

    // 引数はファイル 1 つだけなので、`--` 区切りは持たない。`-foo.md` は `./-foo.md` で開く。
    if let Some(flag) = input.to_str().filter(|arg| arg.starts_with('-')) {
        return match flag {
            "-h" | "--help" => {
                println!("{HELP}");
                Ok(())
            }
            "-V" | "--version" => {
                println!("mdopen {VERSION}");
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
        eprintln!("mdopen: 警告: {warning}");
    }

    let destination = mdopen::output_path(&source);
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
/// 出力先は入力パスから決まる予測可能な位置なので、一時ディレクトリを他の利用者と共有する
/// 環境では、先に symlink を置かれると書き込み先をすり替えられる。
fn write_private(path: &Path, html: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        let mut directory = fs::DirBuilder::new();
        directory.recursive(true);
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(&mut directory, 0o700);
        directory.create(parent)?;
    }

    // symlink 自体を消す。指す先には触らない。
    match fs::remove_file(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }

    let mut options = fs::OpenOptions::new();
    // create_new は O_EXCL で開くので、symlink を辿らず、何かあればそこで失敗する。
    // 必ず新規作成になるため、新規作成にしか効かない mode もそのまま効く。
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);

    options.open(path)?.write_all(html.as_bytes())
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
            Error::Usage => write!(f, "{USAGE}"),
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

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mdopen-test-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
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
}
