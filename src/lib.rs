//! Markdown を HTML 1 枚に変換する。
//!
//! 出力に埋め込むのは、全ページが必要とし欠けると読めなくなるもの (CSS・コードの色) だけ。
//! 一部の入力しか必要とせず欠けても degrade するもの (画像・図) は参照にとどめる。

use std::collections::HashSet;
use std::fmt::{self, Write as _};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use base64::prelude::{BASE64_STANDARD, Engine as _};
use comrak::adapters::CodefenceRendererAdapter;
use comrak::nodes::{AstNode, NodeValue, Sourcepos};
use comrak::options::{Plugins, URLRewriter};
use comrak::plugins::syntect::{SyntectAdapter, SyntectAdapterBuilder};
use comrak::{Arena, Options, format_html_with_plugins, parse_document};
use sha2::{Digest, Sha256};
use syntect::highlighting::ThemeSet;
use syntect::html::{ClassStyle, css_for_theme_with_class_style};

mod sanitize;

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
        base_dir: base_dir.to_path_buf(),
        checked: Mutex::new(HashSet::new()),
        warnings: Mutex::new(Vec::new()),
    });

    let mut options = markdown_options();
    options.extension.image_url_rewriter = Some(images.clone());
    options.extension.link_url_rewriter = Some(Arc::new(LinkResolver { base_dir }));

    let arena = Arena::new();
    let root = parse_document(&arena, markdown, &options);

    let mut has_mermaid = false;
    let mut has_highlightable_code = false;
    let mut raw_html = Vec::new();

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
            NodeValue::HtmlBlock(_) | NodeValue::HtmlInline(_) => raw_html.push(node),
            _ => {}
        }
    }

    let highlighter = has_highlightable_code.then(syntax_highlighter);
    let mermaid = MermaidRenderer;
    let mut plugins = Plugins::default();
    plugins.render.codefence_syntax_highlighter = highlighter.as_ref().map(|(a, _)| a as &_);
    plugins
        .render
        .codefence_renderers
        .insert(MERMAID_LANG.to_string(), &mermaid);

    // 閉じていない生の HTML は、以降の本文を丸ごと飲み込んで消しうる (閉じない
    // `<script>` やコメント、`<template>`、HTML の文脈に置いた `<path>` など)。どれが
    // 消すかは HTML の構文と sanitize の判定の両方で決まるので、推測はしない。
    // まとまりごとに直後へ目印を置き、sanitize 後に目印が消えていたら、そのまとまりを
    // テキストとして表示して変換し直す。
    // 目印は文書から作る。文書に自分自身のハッシュは書けないので、偽の目印は置けない。
    let marker = sentinel_marker(markdown);
    let groups = raw_html_groups(&raw_html);
    for (i, group) in groups.iter().enumerate() {
        let last = group.last().expect("まとまりは空でない");
        push_literal(last, &sentinel(&marker, i));
    }
    let sanitizer = sanitizer(images.clone());
    let mut shown_as_text = vec![false; groups.len()];
    // まず、まとまり単独で目印を消すものを見つける。文書全体の変換し直しは重いので、
    // 他のまとまりと組み合わさって初めて消すものだけを、下の繰り返しに残す。
    for (i, group) in groups.iter().enumerate() {
        let alone: String = group.iter().map(|node| literal_of(node)).collect();
        if !surviving_sentinels(&sanitizer.clean(&alone), &marker).contains(&i) {
            show_as_text(group, &sentinel(&marker, i));
            shown_as_text[i] = true;
        }
    }
    let mut passes = 0;
    let (body, mut html) = loop {
        images.reset();
        let mut body = String::new();
        format_html_with_plugins(root, &options, &mut body, &plugins)
            .expect("String への書き出しは失敗しない");
        let html = sanitizer.clean(&body);
        let survived = surviving_sentinels(&html, &marker);
        let missing: Vec<usize> = (0..groups.len())
            .filter(|&i| !shown_as_text[i] && !survived.contains(&i))
            .collect();
        if missing.is_empty() {
            break (body, html);
        }
        // 最初に消えた目印のまとまりが、後ろの目印もまとめて消している。変換し直す
        // 回数を抑えるため、一定回数を超えたら消えたものをまとめてテキストにする。
        passes += 1;
        let culprits = if passes < MAX_RERENDERS {
            &missing[..1]
        } else {
            &missing[..]
        };
        for &i in culprits {
            show_as_text(&groups[i], &sentinel(&marker, i));
            shown_as_text[i] = true;
        }
    };
    // 目印はどちらにも同じ数だけあるので、取り除いたものには現れない。
    let removed = sanitize::removed_markup(&body, &html);
    strip_sentinels(&mut html, &marker);

    let mut warnings = Vec::new();
    if !removed.is_empty() {
        warnings.push(format!("出力から取り除いた HTML: {}", removed.join(", ")));
    }
    warnings.append(&mut images.warnings.lock().expect("poison しない"));

    let highlight_css = highlighter.as_ref().map(|(_, css)| css.as_str());
    Rendered {
        html: assemble(title, &html, has_mermaid, highlight_css, &warnings),
        warnings,
    }
}

/// 閉じていない生の HTML を 1 つずつ見つけるために変換し直す回数の上限。超えたら、
/// 目印が消えたまとまりをまとめてテキストにする (巻き込まれただけのものも含む)。
const MAX_RERENDERS: usize = 64;

/// 変換後に消えていないかを確かめる目印の、番号の前までの部分。sanitize で残る要素と
/// 属性にする。
fn sentinel_marker(markdown: &str) -> String {
    let digest = Sha256::digest(markdown.as_bytes());
    let mut marker = String::from("<span id=\"mdopen-");
    for byte in &digest[..16] {
        let _ = write!(marker, "{byte:02x}");
    }
    marker.push('-');
    marker
}

fn sentinel(marker: &str, i: usize) -> String {
    format!("{marker}{i}\"></span>")
}

/// 目印の番号と、目印全体の範囲。
fn sentinels<'a>(
    html: &'a str,
    marker: &'a str,
) -> impl Iterator<Item = (usize, usize, usize)> + 'a {
    html.match_indices(marker).filter_map(move |(start, _)| {
        let after = &html[start + marker.len()..];
        let digits = after.bytes().take_while(u8::is_ascii_digit).count();
        let rest = after[digits..].strip_prefix("\"></span>")?;
        let i = after[..digits].parse().ok()?;
        Some((i, start, html.len() - rest.len()))
    })
}

fn surviving_sentinels(html: &str, marker: &str) -> HashSet<usize> {
    sentinels(html, marker).map(|(i, _, _)| i).collect()
}

fn strip_sentinels(html: &mut String, marker: &str) {
    let ranges: Vec<(usize, usize)> = sentinels(html, marker).map(|(_, s, e)| (s, e)).collect();
    for (start, end) in ranges.into_iter().rev() {
        html.replace_range(start..end, "");
    }
}

/// 生の HTML を、HTML ブロック 1 つ、または 1 つのブロックの中のインライン HTML ごとに
/// まとめる。画像の代替テキストの中は HTML として出力されないので除く。
fn raw_html_groups<'a>(raw_html: &[&'a AstNode<'a>]) -> Vec<Vec<&'a AstNode<'a>>> {
    let mut groups: Vec<(&AstNode, Vec<&AstNode>)> = Vec::new();
    for &node in raw_html {
        let container = if matches!(node.data().value, NodeValue::HtmlBlock(_)) {
            node
        } else {
            if node
                .ancestors()
                .any(|ancestor| matches!(ancestor.data().value, NodeValue::Image(_)))
            {
                continue;
            }
            node.ancestors()
                .skip(1)
                .find(|ancestor| ancestor.data().value.block())
                .unwrap_or(node)
        };
        match groups.last_mut() {
            Some((last, members)) if std::ptr::eq(*last, container) => members.push(node),
            _ => groups.push((container, vec![node])),
        }
    }
    groups.into_iter().map(|(_, members)| members).collect()
}

fn literal_of(node: &AstNode) -> String {
    match &node.data().value {
        NodeValue::HtmlBlock(block) => block.literal.clone(),
        NodeValue::HtmlInline(literal) => literal.clone(),
        _ => unreachable!("生の HTML のノードだけを集めている"),
    }
}

fn push_literal(node: &AstNode, text: &str) {
    match &mut node.data_mut().value {
        NodeValue::HtmlBlock(block) => block.literal.push_str(text),
        NodeValue::HtmlInline(literal) => literal.push_str(text),
        _ => unreachable!("生の HTML のノードだけを集めている"),
    }
}

/// まとまりを HTML ではなくテキストとして表示する。目印は外す。HTML ブロックは
/// 改行を保つよう、整形済みのテキストにする。
fn show_as_text(group: &[&AstNode], sentinel: &str) {
    for node in group {
        let mut ast = node.data_mut();
        let (literal, block) = match &mut ast.value {
            NodeValue::HtmlBlock(block) => (&mut block.literal, true),
            NodeValue::HtmlInline(literal) => (literal, false),
            _ => unreachable!("生の HTML のノードだけを集めている"),
        };
        let source = literal.strip_suffix(sentinel).unwrap_or(literal);
        *literal = if block {
            format!("<pre>{}</pre>\n", escape_html(source))
        } else {
            escape_html(source)
        };
    }
}

/// 生の HTML の `src` / `href` を Markdown の画像・リンクと同じに入力ファイル基準の
/// `file://` URL にし、画像なら存在を確かめる sanitizer。
fn sanitizer(images: Arc<ImageResolver>) -> sanitize::Sanitizer {
    sanitize::Sanitizer::new(move |element, attribute, value| {
        // ブラウザと同じく、前後の空白・制御文字と途中のタブ・改行を無視する。
        let url: String = value
            .trim_matches(|c: char| c <= ' ')
            .chars()
            .filter(|c| !matches!(c, '\t' | '\n' | '\r'))
            .collect();
        let is_image = element == "img" && attribute == "src";
        // `file://` の URL (Markdown の画像・リンクを書き換えたものを含む) は書き換えない。
        // 画像なら存在だけ確かめる。Markdown の画像は確認済みなので二重には警告しない。
        if let Some(path) = strip_file_scheme(&url) {
            if is_image && let Some((target, _)) = split_local_reference(path, &images.base_dir) {
                images.check_exists(&url, &target);
            }
            return None;
        }
        let (target, suffix) = split_local_reference(&url, &images.base_dir)?;
        if is_image {
            images.check_exists(&url, &target);
        }
        // comrak の href escape を通らないので、空白等の encode もここで行う。
        Some(percent_encode_url(&format_file_url(&target, suffix)))
    })
}

/// class 方式の syntect と、その色付けの CSS。
///
/// インラインの `style` 属性で色を焼き込む方式は使わない。`style` 属性を許すと生の HTML
/// にも許すことになり、`position: fixed` で警告欄を覆い隠したり、`url()` で読み込みを
/// 起こしたりできてしまう。
fn syntax_highlighter() -> (SyntectAdapter, String) {
    let mut theme = ThemeSet::load_defaults()
        .themes
        .remove(SYNTECT_THEME)
        .expect("組み込みのテーマに含まれる");
    // 背景はページの配色 (style.css) に揃える。テーマの背景と同じ色の指定は、インライン
    // 方式でも出力されなかったので除く。
    if let Some(background) = theme.settings.background.take() {
        for item in &mut theme.scopes {
            if item.style.background == Some(background) {
                item.style.background = None;
            }
        }
    }
    let class_style = ClassStyle::SpacedPrefixed {
        prefix: sanitize::SYNTECT_CLASS_PREFIX,
    };
    let mut css = css_for_theme_with_class_style(&theme, class_style)
        .expect("組み込みのテーマから CSS を作れる");
    // テーマの基準の文字色は `.<接頭辞>code` に出るが、comrak の adapter はその class を
    // どこにも付けない。色付けした `<pre>` に当てる。
    if let Some(color) = theme.settings.foreground {
        let _ = write!(
            css,
            "\n.markdown-body pre.syntax-highlighting {{\n color: #{:02x}{:02x}{:02x};\n}}\n",
            color.r, color.g, color.b
        );
    }
    let adapter = SyntectAdapterBuilder::new()
        .css_with_class_prefix(sanitize::SYNTECT_CLASS_PREFIX)
        // class 方式ではテーマを使わない。渡さないと既定のテーマ一式を読み込む。
        .theme_set(ThemeSet::new())
        .build();
    (adapter, css)
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

    // 生の HTML も出力に通し、変換後の本文全体を sanitize で許可リストに沿って絞る。
    // comrak が拒否リストで落とす URL (javascript: など) も、そこでまとめて落ちる。
    options.render.r#unsafe = true;

    options
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
struct ImageResolver {
    base_dir: PathBuf,
    /// 確かめたパス。同じ画像は書き方が違っても 1 度だけ警告する。
    checked: Mutex<HashSet<PathBuf>>,
    warnings: Mutex<Vec<String>>,
}

impl ImageResolver {
    /// 変換し直す前に、前回の確認結果を捨てる。
    fn reset(&self) {
        self.checked.lock().expect("poison しない").clear();
        self.warnings.lock().expect("poison しない").clear();
    }

    /// 壊れた画像はブラウザ上では理由が分からないので、ここで伝えておく。
    /// `url` は警告に出す綴り。percent encode されていれば復号して見せる。
    fn check_exists(&self, url: &str, target: &Path) {
        let first = self
            .checked
            .lock()
            .expect("poison しない")
            .insert(target.to_path_buf());
        if first && !target.is_file() {
            let shown = percent_decode(url);
            let warning = format!("{}: 画像が見つかりません", shown.as_deref().unwrap_or(url));
            self.warnings.lock().expect("poison しない").push(warning);
        }
    }
}

impl URLRewriter for ImageResolver {
    fn to_html(&self, url: &str) -> String {
        let Some((target, suffix)) = split_local_reference(url, &self.base_dir) else {
            return url.to_owned();
        };
        self.check_exists(url, &target);
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

fn strip_file_scheme(url: &str) -> Option<&str> {
    let rest = url.strip_prefix("file://")?;
    rest.starts_with('/').then_some(rest)
}

/// URL に使えない文字 (空白、非 ASCII など) を percent encode する。comrak の href
/// escape と同じく、`%XX` の形の `%` だけは encode 済みの綴りとして残す。
fn percent_encode_url(url: &str) -> String {
    let bytes = url.as_bytes();
    let mut out = String::with_capacity(url.len());
    for (i, &byte) in bytes.iter().enumerate() {
        let escaped = byte == b'%'
            && bytes.len() > i + 2
            && bytes[i + 1].is_ascii_hexdigit()
            && bytes[i + 2].is_ascii_hexdigit();
        if escaped || byte.is_ascii_alphanumeric() || b"-_.+!*(),#@?=;:/$~&'".contains(&byte) {
            out.push(byte as char);
        } else {
            let _ = write!(out, "%{byte:02X}");
        }
    }
    out
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

fn assemble(
    title: &str,
    body: &str,
    has_mermaid: bool,
    highlight_css: Option<&str>,
    warnings: &[String],
) -> String {
    let highlight_css = highlight_css.unwrap_or_default();
    let mut html = String::with_capacity(body.len() + STYLE_CSS.len() + highlight_css.len() + 2048);
    // CSP のハッシュと出力する要素の本文を同じ値から作る。食い違うとブラウザは
    // 起動スクリプトを拒否する。
    let bootstrap = has_mermaid.then_some(MERMAID_BOOTSTRAP);

    // CSP の <meta> は自分より後ろにしか効かないので、<head> の先頭に置く。
    let _ = writeln!(
        html,
        "<!DOCTYPE html>\n<html>\n<head>\n\
         <meta http-equiv=\"Content-Security-Policy\" content=\"{}\">",
        content_security_policy(bootstrap),
    );
    html.push_str("<meta charset=\"utf-8\">\n");
    html.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n");
    let _ = write!(html, "<title>{}</title>\n<style>\n", escape_html(title));
    html.push_str(STYLE_CSS);
    html.push_str(highlight_css);
    html.push_str("</style>\n</head>\n<body>\n<article class=\"markdown-body\">\n");
    html.push_str(&warning_banner(warnings));
    html.push_str(body);
    html.push_str("</article>\n");

    // Mermaid を含む入力でだけ読み込ませる。
    if let Some(bootstrap) = bootstrap {
        let _ = write!(
            html,
            "<script src=\"{MERMAID_URL}\" integrity=\"{MERMAID_SRI}\" \
             crossorigin=\"anonymous\" defer></script>\n<script>{bootstrap}</script>\n",
        );
    }

    html.push_str("</body>\n</html>\n");
    html
}

/// 生の HTML の除去に見落としがあってもスクリプトを実行させないための多重防御。
///
/// `bootstrap` は出力する起動スクリプトの `<script>` 要素の本文そのもの。無ければ
/// スクリプトを一切許さない。`style-src` と `img-src` は制限しない。mermaid は描画時に
/// `<style>` と `style` 属性を挿入し、Markdown の画像は外部・`data:`・`file://` のどれも使う。
fn content_security_policy(bootstrap: Option<&str>) -> String {
    let script_src = match bootstrap {
        Some(body) => format!(
            "{MERMAID_URL} 'sha256-{}'",
            BASE64_STANDARD.encode(Sha256::digest(body.as_bytes()))
        ),
        None => "'none'".to_string(),
    };
    format!("script-src {script_src}; object-src 'none'; base-uri 'none'; form-action 'none'")
}

/// 描画に失敗した図はソースが残るので、利用者からは変換されなかったことが見える。
///
/// `<script>` 要素の本文そのもの。CSP のハッシュもこの値から計算する。
const MERMAID_BOOTSTRAP: &str = concat!(
    "\n",
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
        // syntect が変換時に色を付けている (JS 不要)
        assert!(html.contains("<pre class=\"syntax-highlighting\">"));
        assert!(html.contains("<span class=\"hl-storage hl-type hl-function hl-rust\">fn</span>"));
    }

    #[test]
    fn highlighted_code_keeps_the_theme_text_color() {
        let html = render_file("plain.md").html;
        assert!(
            html.contains(".markdown-body pre.syntax-highlighting {\n color: #323232;\n}"),
            "{html}"
        );
        // 背景はページの配色に任せる
        let start = html.find("generated by syntect").unwrap();
        let end = start + html[start..].find("</style>").unwrap();
        assert!(!html[start..end].contains("#ffffff"));
    }

    #[test]
    fn highlight_css_matches_the_prefixed_classes() {
        let html = render_file("plain.md").html;
        let start = html.find("generated by syntect").unwrap();
        let css = &html[start..start + html[start..].find("</style>").unwrap()];
        assert!(css.contains(".hl-storage.hl-type"), "{css}");
        // 素の class はページ全体に効くので出さない
        assert!(!css.contains("\n.storage"), "{css}");
        assert!(!css.contains("\n.comment"), "{css}");
    }

    #[test]
    fn alerts_keep_all_their_classes() {
        let html = render_file("plain.md").html;
        for kind in ["note", "tip", "important", "warning", "caution"] {
            assert!(
                html.contains(&format!(
                    "<div class=\"markdown-alert markdown-alert-{kind}\">"
                )),
                "{kind}"
            );
        }
    }

    #[test]
    fn syntect_stays_out_when_no_language_is_given() {
        let html = render_str("```\nplain\n```\n\n    indented\n");
        assert!(!html.contains("syntax-highlighting"));
        assert!(!html.contains("generated by syntect"));
        assert!(html.contains("<code>plain"));
    }

    #[test]
    fn testdata_needs_no_sanitizing() {
        // 許可リストが comrak の出力を覆っていること。
        assert!(render_file("plain.md").warnings.is_empty());
        assert!(render_file("mermaid.md").warnings.is_empty());
        assert_eq!(
            render_file("image.md").warnings,
            ["images/missing.png: 画像が見つかりません"]
        );
    }

    #[test]
    fn data_images_in_raw_html_survive() {
        let png = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNkYAAAAAYAAjCB0C8AAAAASUVORK5CYII=";
        let rendered = render(
            &format!("| a | b |\n| --- | --- |\n| <img src=\"{png}\"> | x |\n"),
            "t.md",
            &testdata(),
        );
        assert!(
            rendered
                .html
                .contains(&format!("<td><img src=\"{png}\"></td>")),
            "{}",
            rendered.html
        );
        assert!(rendered.warnings.is_empty(), "{:?}", rendered.warnings);
    }

    #[test]
    fn harmless_raw_html_survives() {
        let rendered = render(
            "<details>\n<summary>more</summary>\n\nhidden\n\n</details>\n\n\
             line<br>next H<sub>2</sub>O <kbd>Ctrl</kbd>\n",
            "t.md",
            &testdata(),
        );
        for tag in ["<details>", "<summary>", "<br>", "<sub>", "<kbd>"] {
            assert!(rendered.html.contains(tag), "{tag}: {}", rendered.html);
        }
        assert!(rendered.warnings.is_empty(), "{:?}", rendered.warnings);
    }

    #[test]
    fn dangerous_raw_html_is_removed_and_reported() {
        let rendered = render(
            "<script>alert(1)</script>\n\n\
             <img src=\"images/square.png\" onerror=\"alert(2)\">\n\n\
             <a href=\"javascript:alert(3)\">x</a>\n\n\
             <iframe src=\"https://example.com\"></iframe>\n\n\
             <object data=\"x.swf\"></object>\n\n\
             <p style=\"position: fixed\">y</p>\n",
            "t.md",
            &testdata(),
        );
        for needle in [
            "alert(",
            "onerror=",
            "javascript:",
            "<iframe",
            "<object",
            " style=",
        ] {
            assert!(
                !rendered.html.contains(needle),
                "{needle}: {}",
                rendered.html
            );
        }
        assert_eq!(
            rendered.warnings,
            [
                "出力から取り除いた HTML: <script>, <img onerror>, <a href>, <iframe>, <object>, <p style>"
            ]
        );
    }

    #[test]
    fn unclosed_raw_html_is_shown_as_text() {
        let rendered = render(
            "Put a <script> tag in the head.\n\n# Next\n\n\
             More <textarea> text\n\nlast <noscript>\n\nend\n",
            "t.md",
            &testdata(),
        );
        for needle in [
            "<p>Put a &lt;script&gt; tag in the head.</p>",
            "<h1 id=\"next\">",
            "<p>More &lt;textarea&gt; text</p>",
            "<p>end</p>",
        ] {
            assert!(
                rendered.html.contains(needle),
                "{needle}: {}",
                rendered.html
            );
        }
        assert!(rendered.warnings.is_empty(), "{:?}", rendered.warnings);
    }

    #[test]
    fn unterminated_comments_and_quotes_do_not_swallow_the_rest() {
        for markdown in [
            "<details>\n<summary>More</summary>\n<!-- draft\n\nold\n\n-->\n</details>\n\n# Next\n",
            "<div>\n<img src=\"images/square.png\n</div>\n\n# Next\n",
            "a <!-- x --!> <script> y --> b\n\n# Next\n",
            "<plaintext>\n\n# Next\n",
        ] {
            let rendered = render(markdown, "t.md", &testdata());
            assert!(
                rendered.html.contains("<h1 id=\"next\">"),
                "{markdown:?}: {}",
                rendered.html
            );
        }
    }

    #[test]
    fn an_unclosed_template_does_not_hide_the_rest() {
        let rendered = render(
            "Before\n\n<template>\n\n# Next\n\nAfter\n",
            "t.md",
            &testdata(),
        );
        assert!(
            rendered.html.contains("<h1 id=\"next\">"),
            "{}",
            rendered.html
        );
        assert!(rendered.html.contains("<p>After</p>"), "{}", rendered.html);
    }

    #[test]
    fn non_ascii_raw_html_paths_are_encoded_like_markdown() {
        let base = testdata().display().to_string();
        let raw = render_str("<img src=\"images/画像.png\">\n");
        let markdown = render_str("![](images/画像.png)\n");
        let url = format!("src=\"file://{base}/images/%E7%94%BB%E5%83%8F.png\"");
        assert!(raw.contains(&url), "{raw}");
        assert!(markdown.contains(&url), "{markdown}");
    }

    #[test]
    fn unclosed_elements_removed_with_their_content_do_not_hide_the_rest() {
        for markdown in [
            "Pass <path> as the first argument.\n\n# Next\n",
            "Usage:\n\n<path>\n\nFollowing\n\n# Next\n",
            "Put a <template/> here.\n\n# Next\n",
            "<select>\n\n# Next\n",
            "<svg>\n<style><![CDATA[\n  g > path { fill: red }\n</style>\n</svg>\n\n# Next\n",
        ] {
            let rendered = render(markdown, "t.md", &testdata());
            assert!(
                rendered.html.contains("<h1 id=\"next\">"),
                "{markdown:?}: {}",
                rendered.html
            );
        }
        let rendered = render("Pass <path> as the first argument.\n", "t.md", &testdata());
        assert!(
            rendered
                .html
                .contains("<p>Pass &lt;path&gt; as the first argument.</p>"),
            "{}",
            rendered.html
        );
        assert!(rendered.warnings.is_empty(), "{:?}", rendered.warnings);
    }

    #[test]
    fn many_unclosed_elements_are_all_shown_as_text() {
        let markdown: String = (0..MAX_RERENDERS * 3)
            .map(|i| format!("p{i} <path> x{i}\n\n"))
            .collect();
        let rendered = render(&markdown, "t.md", &testdata());
        for i in 0..MAX_RERENDERS * 3 {
            assert!(
                rendered
                    .html
                    .contains(&format!("<p>p{i} &lt;path&gt; x{i}</p>")),
                "{i}: {}",
                rendered.html
            );
        }
    }

    #[test]
    fn sentinels_never_reach_the_output() {
        let rendered = render(
            "# Title <kbd>K</kbd>\n\n![a <b>x</b>](images/square.png) <kbd>Ctrl</kbd>\n\n<div>\n\nx\n\n</div>\n",
            "t.md",
            &testdata(),
        );
        assert!(
            !rendered.html.contains("<span id=\"mdopen-"),
            "{}",
            rendered.html
        );
        assert!(
            rendered.html.contains("<kbd>Ctrl</kbd>"),
            "{}",
            rendered.html
        );
        assert!(rendered.html.contains("<div>"), "{}", rendered.html);
        assert!(
            rendered.html.contains("alt=\"a &lt;b&gt;x&lt;/b&gt;\""),
            "{}",
            rendered.html
        );
        assert!(rendered.warnings.is_empty(), "{:?}", rendered.warnings);
    }

    #[test]
    fn harmless_html_after_unclosed_html_is_kept() {
        let mut markdown: String = (0..10)
            .map(|i| format!("Mention {i} <script> in prose.\n\n"))
            .collect();
        markdown.push_str(
            "Press <kbd>Ctrl</kbd>\n\n<details>\n<summary>s</summary>\n\nx\n\n</details>\n",
        );
        let rendered = render(&markdown, "t.md", &testdata());
        assert!(
            rendered.html.contains("<kbd>Ctrl</kbd>"),
            "{}",
            rendered.html
        );
        assert!(rendered.html.contains("<details>"), "{}", rendered.html);
        assert!(
            rendered
                .html
                .contains("<p>Mention 9 &lt;script&gt; in prose.</p>"),
            "{}",
            rendered.html
        );
    }

    #[test]
    fn an_unclosed_block_is_shown_with_its_line_breaks() {
        let rendered = render(
            "Intro\n\n<!-- TODO: finish\n\n# Heading\n",
            "t.md",
            &testdata(),
        );
        assert!(
            rendered
                .html
                .contains("<pre>&lt;!-- TODO: finish\n\n# Heading\n</pre>"),
            "{}",
            rendered.html
        );
    }

    #[test]
    fn math_fences_need_no_sanitizing() {
        let rendered = render("```math\nE = mc^2\n```\n", "t.md", &testdata());
        assert!(rendered.warnings.is_empty(), "{:?}", rendered.warnings);
    }

    #[test]
    fn only_the_unclosed_group_is_shown_as_text() {
        let rendered = render("Put a <script> tag.\n\n<kbd>K</kbd>\n", "t.md", &testdata());
        assert!(
            rendered.html.contains("<p>Put a &lt;script&gt; tag.</p>"),
            "{}",
            rendered.html
        );
        assert!(
            rendered.html.contains("<p><kbd>K</kbd></p>"),
            "{}",
            rendered.html
        );
    }

    #[test]
    fn warnings_come_from_the_final_rendering_only() {
        let rendered = render(
            "a <img src=\"images/nope.png\"> <template>\n\n# Next\n",
            "t.md",
            &testdata(),
        );
        assert!(
            rendered.html.contains("<h1 id=\"next\">"),
            "{}",
            rendered.html
        );
        assert!(rendered.warnings.is_empty(), "{:?}", rendered.warnings);
    }

    #[test]
    fn missing_image_warnings_show_decoded_paths() {
        let rendered = render("![](file:///nonexistent/画像.png)\n", "t.md", &testdata());
        assert_eq!(
            rendered.warnings,
            ["file:///nonexistent/画像.png: 画像が見つかりません"]
        );
    }

    #[test]
    fn missing_file_url_images_in_markdown_are_reported() {
        let rendered = render("![](file:///nonexistent/x.png)\n", "t.md", &testdata());
        assert_eq!(
            rendered.warnings,
            ["file:///nonexistent/x.png: 画像が見つかりません"]
        );
    }

    #[test]
    fn html_in_image_alt_text_is_not_treated_as_raw_html() {
        let rendered = render(
            "<kbd>K</kbd> ![a <b>x</b>](images/square.png)\n",
            "t.md",
            &testdata(),
        );
        assert!(
            rendered.html.contains("<p><kbd>K</kbd> <img"),
            "{}",
            rendered.html
        );
        assert!(
            rendered.html.contains("alt=\"a &lt;b&gt;x&lt;/b&gt;\""),
            "{}",
            rendered.html
        );
        assert!(rendered.warnings.is_empty(), "{:?}", rendered.warnings);
    }

    #[test]
    fn a_document_cannot_fake_its_own_sentinel() {
        let fake = sentinel(&sentinel_marker("another document"), 0);
        let rendered = render(
            &format!("{fake}<template>\n\n# Next\n"),
            "t.md",
            &testdata(),
        );
        assert!(
            rendered.html.contains("<h1 id=\"next\">"),
            "{}",
            rendered.html
        );
        assert_ne!(sentinel_marker("a"), sentinel_marker("b"));
    }

    #[test]
    fn a_script_running_to_the_end_stays_inert() {
        let rendered = render(
            "Intro\n\n<script>\nconst s = \"<img src=images/nope.png>\";\n",
            "t.md",
            &testdata(),
        );
        assert!(!rendered.html.contains("<img"), "{}", rendered.html);
        assert!(rendered.warnings.is_empty(), "{:?}", rendered.warnings);
    }

    #[test]
    fn a_later_element_does_not_close_an_unclosed_one() {
        let rendered = render(
            "Put a <script> tag.\n\n# Next\n\n<script>x</script>\n\nend\n",
            "t.md",
            &testdata(),
        );
        for needle in ["<h1 id=\"next\">", "<p>end</p>"] {
            assert!(
                rendered.html.contains(needle),
                "{needle}: {}",
                rendered.html
            );
        }
        assert!(!rendered.html.contains(">x<"), "{}", rendered.html);
        assert_eq!(rendered.warnings, ["出力から取り除いた HTML: <script>"]);
    }

    #[test]
    fn closed_raw_text_elements_are_left_alone() {
        let rendered = render(
            "a <style>p { color: red }</style> b\n\n<script>\nx()\n</script>\n\nc\n",
            "t.md",
            &testdata(),
        );
        assert!(!rendered.html.contains("color: red"), "{}", rendered.html);
        assert!(!rendered.html.contains("x()"), "{}", rendered.html);
        assert!(rendered.html.contains("<p>c</p>"), "{}", rendered.html);
        assert_eq!(
            rendered.warnings,
            ["出力から取り除いた HTML: <style>, <script>"]
        );
    }

    #[test]
    fn raw_text_tags_in_comments_and_attribute_values_are_untouched() {
        let rendered = render(
            "<a href=\"notes/<plaintext>.md\" title=\"a <script> b\">n</a> <!-- <plaintext> -->\n",
            "t.md",
            &testdata(),
        );
        assert!(
            rendered.html.contains("notes/%3Cplaintext%3E.md"),
            "{}",
            rendered.html
        );
        assert!(
            rendered.html.contains("title=\"a &lt;script&gt; b\""),
            "{}",
            rendered.html
        );
        assert!(rendered.warnings.is_empty(), "{:?}", rendered.warnings);
    }

    #[test]
    fn links_with_unlisted_schemes_are_dropped() {
        let rendered = render("[a](obsidian://open?vault=x)\n", "t.md", &testdata());
        assert!(rendered.html.contains("<a>a</a>"), "{}", rendered.html);
        assert_eq!(rendered.warnings, ["出力から取り除いた HTML: <a href>"]);
    }

    #[test]
    fn raw_html_paths_point_at_the_source_directory() {
        let rendered = render(
            "<img src=\"images/square.png\"> <img src=\"images/no such.png\">\n\
             <a href=\"sub/other.md#x\">a</a> <a href=\"#anchor\">b</a>\n",
            "t.md",
            &testdata(),
        );
        let base = testdata().display().to_string();
        let html = &rendered.html;
        assert!(
            html.contains(&format!("src=\"file://{base}/images/square.png\"")),
            "{html}"
        );
        // 生の HTML の属性値は comrak の href escape を通らないので、空白もここで encode する
        assert!(
            html.contains(&format!("src=\"file://{base}/images/no%20such.png\"")),
            "{html}"
        );
        assert!(
            html.contains(&format!("href=\"file://{base}/sub/other.md#x\"")),
            "{html}"
        );
        assert!(html.contains("href=\"#anchor\""), "{html}");
        assert_eq!(
            rendered.warnings,
            ["images/no such.png: 画像が見つかりません"]
        );
    }

    #[test]
    fn raw_html_paths_keep_their_escapes() {
        let html = render_str("<a href=\"my%23tag.md\">a</a> <a href=\"a.md?q=100%\">b</a>\n");
        assert!(html.contains("/my%23tag.md\""), "{html}");
        assert!(html.contains("/a.md?q=100%25\""), "{html}");
    }

    #[test]
    fn raw_html_urls_ignore_surrounding_whitespace() {
        let rendered = render(
            "<img src=\" images/square.png \"> <a href=\" https://example.com\">a</a>\n",
            "t.md",
            &testdata(),
        );
        let base = testdata().display().to_string();
        assert!(
            rendered
                .html
                .contains(&format!("src=\"file://{base}/images/square.png\"")),
            "{}",
            rendered.html
        );
        assert!(
            rendered.html.contains("href=\" https://example.com\""),
            "{}",
            rendered.html
        );
        assert!(rendered.warnings.is_empty(), "{:?}", rendered.warnings);
    }

    #[test]
    fn missing_file_url_images_in_raw_html_are_reported() {
        let rendered = render(
            "<img src=\"file:///nonexistent/x.png\">\n",
            "t.md",
            &testdata(),
        );
        assert!(rendered.html.contains("src=\"file:///nonexistent/x.png\""));
        assert_eq!(
            rendered.warnings,
            ["file:///nonexistent/x.png: 画像が見つかりません"]
        );
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
        assert!(
            rendered
                .html
                .contains("<pre class=\"syntax-highlighting\">")
        );
    }

    /// `<head>` の先頭にある CSP の `<meta>` の content を返す。
    fn csp_at_head_start(html: &str) -> &str {
        let prefix = "<head>\n<meta http-equiv=\"Content-Security-Policy\" content=\"";
        let start = html.find(prefix).expect("<head> の先頭に CSP が無い") + prefix.len();
        let len = html[start..].find('"').unwrap();
        &html[start..start + len]
    }

    #[test]
    fn pages_without_diagrams_allow_no_script() {
        let html = render_file("plain.md").html;
        assert_eq!(
            csp_at_head_start(&html),
            "script-src 'none'; object-src 'none'; base-uri 'none'; form-action 'none'"
        );
    }

    #[test]
    fn mermaid_pages_allow_only_the_library_and_the_bootstrap() {
        let html = render_file("mermaid.md").html;
        let csp = csp_at_head_start(&html);

        // 出力 HTML から起動スクリプトの本文を取り出して、ブラウザと同じ計算で照合する。
        let open = "<script>";
        assert_eq!(
            html.matches(open).count(),
            1,
            "インラインスクリプトは起動スクリプトだけ"
        );
        let start = html.find(open).expect("起動スクリプトが無い") + open.len();
        let body = &html[start..start + html[start..].find("</script>").unwrap()];
        let hash = BASE64_STANDARD.encode(Sha256::digest(body.as_bytes()));

        assert_eq!(
            csp,
            format!(
                "script-src {MERMAID_URL} 'sha256-{hash}'; \
                 object-src 'none'; base-uri 'none'; form-action 'none'"
            )
        );
    }

    #[test]
    fn mermaid_language_is_matched_case_insensitively() {
        let html = render_str("```Mermaid\nflowchart TD\n```\n");
        assert!(html.contains("<pre class=\"mermaid\">flowchart TD"));
        assert!(!html.contains("syntax-highlighting"));
    }

    #[test]
    fn mermaid_diagram_source_is_escaped() {
        let html = render_str("```mermaid\nA[\"<b> & </b>\"]\n```\n");
        // 再シリアライズで `&quot;` は `"` に戻るが、タグにはならない
        assert!(html.contains("A[\"&lt;b&gt; &amp; &lt;/b&gt;\"]"), "{html}");
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
        let rendered = render(
            "<!-- note --><script>x</script>after\n",
            "t.md",
            &testdata(),
        );
        assert_eq!(rendered.warnings, ["出力から取り除いた HTML: <script>"]);
    }

    #[test]
    fn multiple_html_comments_need_no_warning() {
        let rendered = render("<!-- a --><!-- b -->\n", "t.md", &testdata());
        assert!(rendered.warnings.is_empty(), "{:?}", rendered.warnings);
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
