# mdopen

[English](README.md)

Markdown を HTML 1 枚に変換して、その置き場を返す。

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

## 制限

いずれも設計上の選択で、知らずに使うと驚くもの。

- 生の HTML は出力に含めない。`<details>` や `<br>` はタグごと落ちる。変換したページを `file://` で開くため、Markdown 中の `<script>` や `<img onerror>` を通すと、ローカルファイルを読める文脈で実行されてしまう。落としたときはページの先頭に警告が出る
- mermaid の図を含む文書を開くと、描画ライブラリを jsdelivr から取得する。取得内容は SRI で固定しているが、接続そのものは発生する。図を含まない文書では JavaScript を一切読み込まない
- 画像は埋め込まずに参照する。元のファイルを移動・削除すると、ページの表示も壊れる
- 出力は自動削除しない。所有者だけが読める権限で `$TMPDIR/mdopen/` に残り続ける

## ライセンス

MIT または Apache-2.0 のデュアルライセンス。
