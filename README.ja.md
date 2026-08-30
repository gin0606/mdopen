# mdopen

[English](README.md)

Markdown を HTML 1 枚に変換する。

`mdhtml` はページを書き出して、その置き場を返す。開くかどうかは受け取った側が決める。

```
mdhtml file.md
```

Finder で md ファイルを `mdopen.app` に渡すと、変換して既定のブラウザで開く。

## インストール

`mdhtml` コマンドを入れる:

```
brew install gin0606/tap/mdhtml
```

`mdopen.app` は Finder から開くときだけ要るもので、別の cask になっている:

```
brew install --cask gin0606/tap/mdopen
```

`mdopen.app` は md ファイルの既定のアプリにはならないので、macOS が元から使っているアプリのままになる。「このアプリケーションで開く」から選ぶか、アプリにファイルをドロップする。

## 変換と表示を 1 語で

`mdhtml` はパスを返すところで止まるので、近道は自分で名付ける。パスは一度変数に受けて、変換の失敗をそこで止める。そのまま繋ぐと失敗が `open` まで素通りし、bash 形は何も開かないまま終了コード 0 を返し、fish 形は `open` の usage で診断が埋まる。

```fish
function mdopen
    set -l page (mdhtml $argv) || return
    open $page
end
```

```bash
mdopen() { local page; page=$(mdhtml "$1") && open "$page"; }
```

## 制限

いずれも設計上の選択で、知らずに使うと驚くもの。

- 生の HTML は出力に含めない。`<details>` や `<br>` はタグごと落ちる。変換したページを `file://` で開くため、Markdown 中の `<script>` や `<img onerror>` を通すと、ローカルファイルを読める文脈で実行されてしまう。落としたときはページの先頭に警告が出る
- mermaid の図を含む文書を開くと、描画ライブラリを jsdelivr から取得する。取得内容は SRI で固定しているが、接続そのものは発生する。図を含まない文書では JavaScript を一切読み込まない
- 画像は埋め込まずに参照する。元のファイルを移動・削除すると、ページの表示も壊れる
- 出力は自動削除しない。所有者だけが読める権限で `$TMPDIR/mdopen/` に残り続ける

## ライセンス

MIT または Apache-2.0 のデュアルライセンス。
