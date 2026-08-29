# mdo の表示確認

`mdo` が GFM の主な記法を描画できるかを目で見て確かめるためのファイル。

## 段落と強調

**太字**、*斜体*、~~打ち消し~~、`インラインコード`。
裸の URL も自動でリンクになる: https://github.com/gin0606/mdo

絵文字ショートコード: :tada: :rocket: :sparkles:

## リスト

- 箇条書き
- ネスト
  - 子
    - 孫
1. 番号付き
2. その次

## タスクリスト

- [x] 実装する
- [ ] 動作を確かめる
- [ ] コミットする

## 表

| 領域 | 採用 | 備考 |
| --- | --- | --- |
| Markdown パーサ | comrak | GFM 拡張が一通り揃う |
| ハイライト | syntect | 変換時に色を焼き込む |
| ブラウザ起動 | opener | `open(1)` を叩く |

## Alert

> [!NOTE]
> 補足。plan ファイルで頻出する。

> [!TIP]
> 助言。

> [!IMPORTANT]
> 重要。

> [!WARNING]
> 警告。

> [!CAUTION]
> 危険。

## コードブロック

```rust
fn main() {
    let greeting = "hello";
    println!("{greeting}, world");
}
```

```sh
cargo build --release
```

```
言語指定なしのブロック。
```

## リンク

隣のファイルへの相対リンク: [image.md](./image.md)

ページ内アンカー: [表へ戻る](#表)

## 引用と水平線

> 引用文。
> 2 行目。

---

## 脚注

本文からの参照[^1]と、もう一つ[^note]。

[^1]: 1 つ目の脚注。
[^note]: 名前付きの脚注。
