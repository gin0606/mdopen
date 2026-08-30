//! 変換結果の置き場は標準出力の 1 行として渡す。この形が `mdopen.app` のランチャと
//! README のシェル関数の両方の前提になっていて、崩れても両者は黙って壊れる。

use std::path::Path;
use std::process::Command;

fn run(argument: &str) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_mdhtml"))
        .arg(argument)
        .output()
        .expect("mdhtml を起動できません")
}

#[test]
fn a_converted_document_is_reported_as_one_line() {
    let output = run("testdata/plain.md");
    assert!(output.status.success(), "{output:?}");

    let stdout = String::from_utf8(output.stdout).expect("標準出力が UTF-8 ではありません");
    assert!(stdout.ends_with('\n'), "行が閉じていません: {stdout:?}");

    let mut lines = stdout.lines();
    let page = lines.next().expect("パスが返っていません");
    assert_eq!(
        lines.next(),
        None,
        "標準出力が 2 行以上あります: {stdout:?}"
    );
    // ランチャは cwd が / の LaunchServices 起動なので、相対パスでは開けない。
    assert!(
        Path::new(page).is_absolute(),
        "{page} が絶対パスではありません"
    );
    assert!(Path::new(page).is_file(), "{page} が書き出されていません");
}

#[test]
fn a_failure_leaves_stdout_empty() {
    let output = run("testdata/この名前のファイルは無い.md");
    assert!(!output.status.success());
    assert!(
        output.stdout.is_empty(),
        "失敗したのにパスを返しています: {:?}",
        output.stdout
    );
}
