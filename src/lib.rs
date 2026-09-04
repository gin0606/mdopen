//! Markdown を HTML 1 枚に変換する。
//!
//! 出力に埋め込むのは、全ページが必要とし欠けると読めなくなるもの (CSS・コードの色) だけ。
//! 一部の入力しか必要とせず欠けても degrade するもの (画像・図) は参照にとどめる。

use std::fmt::{self, Write as _};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use comrak::adapters::CodefenceRendererAdapter;
use comrak::nodes::{NodeValue, Sourcepos};
use comrak::options::{Plugins, URLRewriter};
use comrak::plugins::syntect::SyntectAdapter;
use comrak::{Arena, Options, format_html_with_plugins, parse_document};
use sha2::{Digest, Sha256};

const STYLE_CSS: &str = include_str!("../assets/style.css");

/// Mermaid の配信元。読み込めなくても `<pre class="mermaid">` に図のソースが残る。
const MERMAID_URL: &str = "https://cdn.jsdelivr.net/npm/mermaid@11.12.0/dist/mermaid.min.js";
/// `MERMAID_URL` の中身を固定する。配信元が差し替えられても別物は実行させない。
///
/// URL のバージョンを変えたら必ず取り直す。片方だけだと SRI 不一致で図が黙って出なくなる。
/// `curl -sSL <URL> | openssl dgst -sha384 -binary | openssl base64 -A`
const MERMAID_SRI: &str = "sha384-o+g/BxPwhi0C3RK7oQBxQuNimeafQ3GE/ST4iT2BxVI4Wzt60SH4pq9iXVYujjaS";

/// syntect のテーマ。ライト固定なので暗いテーマは持たない。
const SYNTECT_THEME: &str = "InspiredGitHub";

/// `<pre class="mermaid">` に差し替えるコードフェンスの言語名。
const MERMAID_LANG: &str = "mermaid";

/// 変換結果。
pub struct Rendered {
    pub html: String,
    /// 変換は続けたが利用者に伝えるべきこと (見つからなかった画像等)。
    pub warnings: Vec<String>,
}

/// Markdown を HTML に変換する。
///
/// `base_dir` は画像とリンクの相対パスを解決する基準ディレクトリ (入力ファイルの親)。
/// 画像が見つからないといった不足は変換を止めず、`warnings` に理由を積む。
pub fn render(markdown: &str, title: &str, base_dir: &Path) -> Rendered {
    let images = Arc::new(ImageResolver {
        base_dir,
        warnings: Mutex::new(Vec::new()),
    });

    let mut options = markdown_options();
    options.extension.image_url_rewriter = Some(images.clone());
    options.extension.link_url_rewriter = Some(Arc::new(LinkResolver { base_dir }));

    let arena = Arena::new();
    let root = parse_document(&arena, markdown, &options);

    let mut has_mermaid = false;
    let mut has_highlightable_code = false;
    let mut dropped_html = false;

    for node in root.descendants() {
        let mut ast = node.data_mut();
        match &mut ast.value {
            NodeValue::CodeBlock(block) => match language_of(&block.info) {
                Some(lang) if lang.eq_ignore_ascii_case(MERMAID_LANG) => {
                    has_mermaid = true;
                    // comrak の codefence renderer は言語名を完全一致で引く。
                    block.info = MERMAID_LANG.to_string();
                }
                // 言語指定の無いフェンスに syntect を通しても色は付かない。
                // テーマと構文定義のロードに数 ms かかるので、色が付く入力でだけ行う
                // (言語ごとの正規表現コンパイルのほうが重いが、そちらは避けようがない)。
                Some(_) => has_highlightable_code = true,
                None => {}
            },
            NodeValue::Image(link) | NodeValue::Link(link) => {
                if let Some(path) = strip_file_scheme(&link.url) {
                    link.url = path.to_string();
                }
            }
            NodeValue::HtmlBlock(block) => dropped_html |= !contains_only_comments(&block.literal),
            NodeValue::HtmlInline(literal) => dropped_html |= !contains_only_comments(literal),
            _ => {}
        }
    }

    let highlighter = has_highlightable_code.then(|| SyntectAdapter::new(Some(SYNTECT_THEME)));
    let mermaid = MermaidRenderer;
    let mut plugins = Plugins::default();
    plugins.render.codefence_syntax_highlighter = highlighter.as_ref().map(|a| a as &_);
    plugins
        .render
        .codefence_renderers
        .insert(MERMAID_LANG.to_string(), &mermaid);

    let mut body = String::new();
    format_html_with_plugins(root, &options, &mut body, &plugins)
        .expect("String への書き出しは失敗しない");

    let mut warnings = Vec::new();
    if dropped_html {
        warnings.push("生の HTML は出力に含めていません".to_string());
    }
    warnings.append(&mut images.warnings.lock().expect("poison しない"));

    Rendered {
        html: assemble(title, &body, has_mermaid, &warnings),
        warnings,
    }
}

/// 入力ファイルの絶対パスから、決め打ちの出力先を返す。
///
/// 同じファイルは常に同じパスに書き出されるので、ブラウザのタブを使い回せる。
pub fn output_path(source: &Path) -> PathBuf {
    let digest = Sha256::digest(source.as_os_str().as_encoded_bytes());
    let mut name = String::with_capacity(21);
    for byte in &digest[..8] {
        let _ = write!(name, "{byte:02x}");
    }
    name.push_str(".html");
    std::env::temp_dir().join("mdopen").join(name)
}

fn markdown_options() -> Options<'static> {
    let mut options = Options::default();

    options.extension.table = true;
    options.extension.strikethrough = true;
    options.extension.tasklist = true;
    options.extension.autolink = true;
    options.extension.footnotes = true;
    options.extension.alerts = true;
    options.extension.shortcodes = true;
    options.extension.header_id_prefix = Some(String::new());

    // スタイルシートがタスクリストの見た目に使うクラスを出させる。
    options.render.tasklist_classes = true;

    // render.unsafe は既定の false のまま。生の HTML を通すと、md に書かれた <script> や
    // <img onerror> が file:// のページで実行される。想定入力は簡素な Markdown なので、
    // <details> や <br> がタグごと落ちる代償のほうが軽い。

    options
}

/// 出力から落ちても利用者に伝える必要のない、コメントだけの生 HTML か。
fn contains_only_comments(mut literal: &str) -> bool {
    loop {
        literal = literal.trim_start();
        let Some(comment) = literal.strip_prefix("<!--") else {
            return false;
        };
        let Some(end) = comment.find("-->") else {
            return false;
        };
        literal = &comment[end + 3..];
        if literal.trim().is_empty() {
            return true;
        }
    }
}

/// コードフェンスの言語名。
fn language_of(info: &str) -> Option<&str> {
    info.split_whitespace().next()
}

/// ` ```mermaid ` を `<pre class="mermaid">` にする。mermaid.js が拾う形。
struct MermaidRenderer;

impl CodefenceRendererAdapter for MermaidRenderer {
    fn write(
        &self,
        output: &mut dyn fmt::Write,
        _lang: &str,
        _meta: &str,
        code: &str,
        _sourcepos: Option<Sourcepos>,
    ) -> fmt::Result {
        write!(output, "<pre class=\"mermaid\">{}</pre>", escape_html(code))
    }
}

/// 画像の参照を絶対 `file://` URL にする。
///
/// リンクと同じ機構に揃えている。埋め込まないので、参照先が消えれば画像も壊れる。
struct ImageResolver<'a> {
    base_dir: &'a Path,
    warnings: Mutex<Vec<String>>,
}

impl URLRewriter for ImageResolver<'_> {
    fn to_html(&self, url: &str) -> String {
        let Some((target, suffix)) = split_local_reference(url, self.base_dir) else {
            return url.to_owned();
        };

        // 壊れた画像はブラウザ上では理由が分からないので、ここで伝えておく。
        if !target.is_file() {
            let warning = format!("{url}: 画像が見つかりません");
            let mut warnings = self.warnings.lock().expect("poison しない");
            if !warnings.contains(&warning) {
                warnings.push(warning);
            }
        }

        format_file_url(&target, suffix)
    }
}

/// 相対リンクを絶対 `file://` URL にする。出力が入力とは別ディレクトリに置かれるため。
struct LinkResolver<'a> {
    base_dir: &'a Path,
}

impl URLRewriter for LinkResolver<'_> {
    fn to_html(&self, url: &str) -> String {
        match split_local_reference(url, self.base_dir) {
            Some((target, suffix)) => format_file_url(&target, suffix),
            None => url.to_owned(),
        }
    }
}

/// ローカルのファイルを指す参照を、実際のパスと URL の suffix (`?` `#` 以降) に割る。
/// 書き換える対象でなければ `None`。
fn split_local_reference<'a>(url: &'a str, base_dir: &Path) -> Option<(PathBuf, &'a str)> {
    // ページ内アンカーは書き換えると別文書へ飛んでしまう。
    if url.is_empty() || url.starts_with('#') || is_external_url(url) {
        return None;
    }

    let split = url.find(['?', '#']).unwrap_or(url.len());
    let (path, suffix) = url.split_at(split);
    let decoded = percent_decode(path);
    let target = resolve(decoded.as_deref().unwrap_or(path), base_dir);
    target.is_absolute().then_some((target, suffix))
}

fn format_file_url(target: &Path, suffix: &str) -> String {
    // 空白等の percent encode は comrak の href escape に任せる (二重に掛けないため) が、
    // `#` `?` `%` は素通しされるので、パスの一部であることをここで示す。
    // 復号しなかった綴りも literal なパスとして扱うので、同じに揃える。
    let path = escape_url_delimiters(&target.to_string_lossy());
    format!("file://{path}{suffix}")
}

fn escape_url_delimiters(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for c in path.chars() {
        match c {
            '%' => out.push_str("%25"),
            '#' => out.push_str("%23"),
            '?' => out.push_str("%3F"),
            _ => out.push(c),
        }
    }
    out
}

/// comrak が `unsafe` を切ると `file:` URL を出力から消すので、書き換え前にパスへ戻す。
fn strip_file_scheme(url: &str) -> Option<&str> {
    let rest = url.strip_prefix("file://")?;
    rest.starts_with('/').then_some(rest)
}

fn resolve(reference: &str, base_dir: &Path) -> PathBuf {
    let path = Path::new(reference);
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        base_dir.join(path)
    };
    // `./` を畳んでおく。URL に出したときに読める形にするため。
    joined.components().collect()
}

/// ローカルのファイルを指さない URL か。
///
/// Windows のドライブレター (`C:\...`) をスキームと誤認しないよう、2 文字以上を要求する。
fn is_external_url(url: &str) -> bool {
    // `//example.com/x` はホストから始まる URL であってパスではない。
    if url.starts_with("//") {
        return true;
    }
    let Some((scheme, _)) = url.split_once(':') else {
        return false;
    };
    scheme.len() >= 2
        && scheme.starts_with(|c: char| c.is_ascii_alphabetic())
        && scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
}

/// percent encode を復号する。`%` を含まない入力と不正なエスケープはどちらも `None`。
fn percent_decode(input: &str) -> Option<String> {
    if !input.contains('%') {
        return None;
    }

    let mut out = Vec::with_capacity(input.len());
    let mut bytes = input.bytes();
    while let Some(byte) = bytes.next() {
        if byte != b'%' {
            out.push(byte);
            continue;
        }
        let hi = bytes.next()?;
        let lo = bytes.next()?;
        let hex = |b: u8| (b as char).to_digit(16);
        out.push((hex(hi)? * 16 + hex(lo)?) as u8);
    }
    String::from_utf8(out).ok()
}

/// 警告をページの先頭に出す。標準エラーは Finder からの起動では捨てられるので、
/// 変換したページ自身が唯一確実に届く経路になる。
fn warning_banner(warnings: &[String]) -> String {
    if warnings.is_empty() {
        return String::new();
    }

    let mut out = String::from("<aside class=\"mdopen-warnings\">\n<ul>\n");
    for warning in warnings {
        // 警告文には md 由来の文字列が入る。素通しすると file:// のページで実行される。
        let _ = writeln!(out, "<li>{}</li>", escape_html(warning));
    }
    out.push_str("</ul>\n</aside>\n");
    out
}

fn assemble(title: &str, body: &str, has_mermaid: bool, warnings: &[String]) -> String {
    let mut html = String::with_capacity(body.len() + STYLE_CSS.len() + 1024);

    html.push_str("<!DOCTYPE html>\n<html>\n<head>\n<meta charset=\"utf-8\">\n");
    html.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n");
    let _ = write!(html, "<title>{}</title>\n<style>\n", escape_html(title));
    html.push_str(STYLE_CSS);
    html.push_str("</style>\n</head>\n<body>\n<article class=\"markdown-body\">\n");
    html.push_str(&warning_banner(warnings));
    html.push_str(body);
    html.push_str("</article>\n");

    // Mermaid を含む入力でだけ読み込ませる。
    if has_mermaid {
        let _ = write!(
            html,
            "<script src=\"{MERMAID_URL}\" integrity=\"{MERMAID_SRI}\" \
             crossorigin=\"anonymous\" defer></script>\n<script>\n{MERMAID_BOOTSTRAP}</script>\n",
        );
    }

    html.push_str("</body>\n</html>\n");
    html
}

/// 描画に失敗した図はソースが残るので、利用者からは変換されなかったことが見える。
const MERMAID_BOOTSTRAP: &str = concat!(
    "window.addEventListener(\"load\", function () {\n",
    "  if (typeof mermaid === \"undefined\") return;\n",
    "  try {\n",
    "    mermaid.initialize({ startOnLoad: false });\n",
    "    mermaid.run({ querySelector: \"pre.mermaid\", suppressErrors: true });\n",
    "  } catch (error) {\n",
    "    console.error(error);\n",
    "  }\n",
    "});\n",
);

fn escape_html(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn testdata() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata")
    }

    fn render_file(name: &str) -> Rendered {
        let path = testdata().join(name);
        let markdown = std::fs::read_to_string(&path).unwrap();
        render(&markdown, name, path.parent().unwrap())
    }

    fn render_str(markdown: &str) -> String {
        render(markdown, "t.md", &testdata()).html
    }

    #[test]
    fn plain_markdown_has_no_script() {
        let html = render_file("plain.md").html;
        assert!(
            !html.contains("<script"),
            "図も画像も無い入力に JS が混ざった"
        );
    }

    #[test]
    fn plain_markdown_renders_gfm_extensions() {
        let html = render_file("plain.md").html;
        assert!(html.contains("<table>"));
        assert!(html.contains("task-list-item"));
        assert!(html.contains("markdown-alert-warning"));
        assert!(html.contains("data-footnotes"));
        assert!(html.contains("🎉"));
        assert!(html.contains("<del>"));
        // syntect が色を焼き込んでいる (JS 不要)
        assert!(html.contains("<pre style=\"background-color:"));
    }

    #[test]
    fn syntect_stays_out_when_no_language_is_given() {
        let html = render_str("```\nplain\n```\n\n    indented\n");
        assert!(!html.contains("<pre style=\"background-color:"));
        assert!(html.contains("<code>plain"));
    }

    #[test]
    fn raw_html_never_reaches_the_output() {
        let html =
            render_str("<script>alert(1)</script>\n\n<img src=\"x\" onerror=\"alert(1)\">\n");
        assert!(!html.contains("<script>alert"));
        assert!(!html.contains("onerror"));
    }

    #[test]
    fn mermaid_fence_becomes_pre_and_pulls_in_script() {
        let rendered = render_file("mermaid.md");
        assert!(
            rendered
                .html
                .contains("<pre class=\"mermaid\">flowchart TD")
        );
        assert!(rendered.html.contains(&format!("src=\"{MERMAID_URL}\"")));
        assert!(
            rendered
                .html
                .contains(&format!("integrity=\"{MERMAID_SRI}\""))
        );
        assert!(rendered.html.contains("crossorigin=\"anonymous\""));
        assert!(rendered.html.contains("mermaid.run("));
        // mermaid があってもコードブロックのハイライトは効いたまま
        assert!(rendered.html.contains("<pre style=\"background-color:"));
    }

    #[test]
    fn mermaid_language_is_matched_case_insensitively() {
        let html = render_str("```Mermaid\nflowchart TD\n```\n");
        assert!(html.contains("<pre class=\"mermaid\">flowchart TD"));
        assert!(!html.contains("<pre style=\"background-color:"));
    }

    #[test]
    fn mermaid_diagram_source_is_escaped() {
        let html = render_str("```mermaid\nA[\"<b> & </b>\"]\n```\n");
        assert!(html.contains("A[&quot;&lt;b&gt; &amp; &lt;/b&gt;&quot;]"));
    }

    #[test]
    fn local_images_point_at_the_source_directory() {
        let rendered = render_file("image.md");
        let base = testdata().display().to_string();
        assert!(
            rendered
                .html
                .contains(&format!("src=\"file://{base}/images/square.png\""))
        );
        assert!(
            rendered
                .html
                .contains(&format!("src=\"file://{base}/images/circle.svg\""))
        );
        // リモート URL は触らない
        assert!(
            rendered
                .html
                .contains("src=\"https://img.shields.io/badge/mdopen-markdown-blue\"")
        );
        // 見つからない画像はブラウザ上で壊れるだけなので、理由を伝える
        assert_eq!(
            rendered.warnings,
            ["images/missing.png: 画像が見つかりません"]
        );
    }

    #[test]
    fn image_urls_keep_their_query_and_fragment() {
        let html = render_str("![](images/square.png?v=1)\n\n![](images/circle.svg#shape)\n");
        assert!(html.contains("/images/square.png?v=1\""), "{html}");
        assert!(html.contains("/images/circle.svg#shape\""), "{html}");
    }

    #[test]
    fn percent_encoded_image_paths_resolve() {
        let html = render_str("![](images%2Fsquare.png)\n");
        assert!(html.contains("/images/square.png\""), "{html}");
    }

    #[test]
    fn relative_links_point_back_at_the_source_directory() {
        let html = render_str("[a](./sub/other.md)\n\n[b](#anchor)\n\n[c](https://example.com)\n");
        let base = testdata();
        assert!(html.contains(&format!("href=\"file://{}/sub/other.md\"", base.display())));
        // ページ内アンカーと外部 URL は書き換えない
        assert!(html.contains("href=\"#anchor\""));
        assert!(html.contains("href=\"https://example.com\""));
    }

    #[test]
    fn external_url_detection_keeps_relative_paths() {
        assert!(is_external_url("https://example.com/a.png"));
        assert!(is_external_url("data:image/png;base64,AAAA"));
        assert!(is_external_url("//example.com/a.png"));
        assert!(!is_external_url("images/a.png"));
        assert!(!is_external_url("./a.png"));
        assert!(!is_external_url("C:/images/a.png"));
    }

    #[test]
    fn absolute_file_urls_survive_the_round_trip() {
        let square = testdata().join("images/square.png");
        let html = render_str(&format!("![](file://{})\n", square.display()));
        assert!(
            html.contains(&format!("src=\"file://{}\"", square.display())),
            "{html}"
        );
    }

    #[test]
    fn link_paths_keep_their_delimiters_escaped() {
        let html = render_str("[a](my%23tag.md)\n");
        assert!(html.contains("/my%23tag.md\""), "{html}");
    }

    #[test]
    fn link_paths_are_escaped_below_the_base_directory_too() {
        // 基準ディレクトリ側に `#` があっても、そこから先がフラグメント扱いにならないこと。
        let base = testdata().join("note#1");
        let html = render("[a](other.md)\n", "t.md", &base);
        assert!(html.html.contains("/note%231/other.md\""), "{}", html.html);
    }

    #[test]
    fn html_comments_do_not_count_as_dropped_html() {
        let rendered = render(
            "text\n\n<!-- TOC -->\n\nmore <!-- x --> here\n",
            "t.md",
            &testdata(),
        );
        assert!(rendered.warnings.is_empty(), "{:?}", rendered.warnings);
    }

    #[test]
    fn html_after_a_comment_is_reported() {
        let rendered = render("<!-- note --><br>after\n", "t.md", &testdata());
        assert_eq!(rendered.warnings, ["生の HTML は出力に含めていません"]);
    }

    #[test]
    fn multiple_html_comments_need_no_warning() {
        let rendered = render("<!-- a --><!-- b -->\n", "t.md", &testdata());
        assert!(rendered.warnings.is_empty(), "{:?}", rendered.warnings);
    }

    #[test]
    fn dropped_raw_html_is_reported() {
        let rendered = render(
            "<details>x</details>\n\nline<br>next\n",
            "t.md",
            &testdata(),
        );
        assert!(!rendered.html.contains("<details>"));
        assert_eq!(rendered.warnings, ["生の HTML は出力に含めていません"]);
    }

    #[test]
    fn repeated_images_warn_once() {
        let rendered = render(
            "![](images/missing.png)\n\n![](images/missing.png)\n",
            "t.md",
            &testdata(),
        );
        assert_eq!(rendered.warnings.len(), 1);
    }

    #[test]
    fn warnings_reach_the_page_itself() {
        let rendered = render("![](images/missing.png)\n", "t.md", &testdata());
        assert!(rendered.html.contains("<aside class=\"mdopen-warnings\">"));
        assert!(
            rendered
                .html
                .contains("images/missing.png: 画像が見つかりません")
        );
    }

    #[test]
    fn a_document_without_warnings_gets_no_banner() {
        let html = render_file("plain.md").html;
        assert!(!html.contains("<aside class=\"mdopen-warnings\">"));
    }

    #[test]
    fn warning_text_is_escaped() {
        let html = render_str("![](a<script>alert(1)</script>.png)\n");
        assert!(html.contains("<aside class=\"mdopen-warnings\">"), "{html}");
        assert!(!html.contains("<script>alert"), "{html}");
    }

    #[test]
    fn output_path_is_stable_and_unique() {
        let a = output_path(Path::new("/tmp/a.md"));
        let b = output_path(Path::new("/tmp/b.md"));
        assert_eq!(a, output_path(Path::new("/tmp/a.md")));
        assert_ne!(a, b);
        assert_eq!(a.file_name().unwrap().len(), "0123456789abcdef.html".len());
    }
}
