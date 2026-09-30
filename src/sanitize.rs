//! comrak が出力した本文を、許可リストにあるタグ・属性・URL スキームだけに絞る。
//!
//! 変換したページは `file://` で開かれるので、生の HTML の `<script>` や `onerror` を
//! 通すとローカルファイルを読める文脈で実行されうる。

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};

use html5ever::tendril::TendrilSink as _;
use html5ever::{ParseOpts, QualName, local_name, ns, parse_fragment};
use markup5ever_rcdom::{Handle, NodeData, RcDom};

/// syntect が出す class に付ける接頭辞。無いと `.comment` や `.string` のような素の
/// class がページ全体に効く。
pub(crate) const SYNTECT_CLASS_PREFIX: &str = "hl-";

/// comrak (と syntect の adapter) が出す class のうち、有効にしている拡張の分。
const COMRAK_CLASSES: &[&str] = &[
    "anchor",
    "contains-task-list",
    "task-list-item",
    "task-list-item-checkbox",
    "markdown-alert",
    "markdown-alert-title",
    "markdown-alert-note",
    "markdown-alert-tip",
    "markdown-alert-important",
    "markdown-alert-warning",
    "markdown-alert-caution",
    "footnotes",
    "footnote-ref",
    "footnote-backref",
    "mermaid",
    "syntax-highlighting",
];

/// コードフェンスの `<code>` に付く言語名の class の接頭辞。
const LANGUAGE_CLASS_PREFIX: &str = "language-";

/// `data:` を許す画像の形式。ほかの形式 (特に `image/svg+xml` と `text/html`) は、
/// 開いたときに文書として解釈されうるので通さない。
const DATA_IMAGE_TYPES: &[&str] = &["image/png", "image/gif", "image/jpeg", "image/webp"];

pub(crate) struct Sanitizer {
    builder: ammonia::Builder<'static>,
}

impl Sanitizer {
    /// `rewrite_local` は `src` / `href` の値を受け取り、ローカルのファイルを指すなら
    /// 書き換えた値を返す。生の HTML の相対パスを Markdown の画像・リンクと同じに扱うため。
    pub(crate) fn new(
        rewrite_local: impl Fn(&str, &str, &str) -> Option<String> + Send + Sync + 'static,
    ) -> Self {
        Self {
            builder: builder(rewrite_local),
        }
    }

    /// `body` を許可リストに沿って絞る。
    pub(crate) fn clean(&self, body: &str) -> String {
        self.builder.clean(body).to_string()
    }
}

fn builder(
    rewrite_local: impl Fn(&str, &str, &str) -> Option<String> + Send + Sync + 'static,
) -> ammonia::Builder<'static> {
    let mut builder = ammonia::Builder::default();
    builder
        .add_tags(["input", "section"])
        // ブラウザでも表示されない中身なので、テキストとして残さず要素ごと消す。
        .add_clean_content_tags(["noscript", "iframe", "noembed", "noframes", "title"])
        .add_generic_attributes([
            "id",
            "class",
            "aria-label",
            "data-footnotes",
            "data-footnote-ref",
            "data-footnote-backref",
            "data-footnote-backref-idx",
            "data-heading-content",
            "data-math-style",
        ])
        .add_tag_attributes("input", ["type", "disabled", "checked"])
        // 生の HTML からテキスト入力欄を置けないよう、どの input もタスクリストと同じ
        // 無効化したチェックボックスにする。
        .set_tag_attribute_value("input", "type", "checkbox")
        .set_tag_attribute_value("input", "disabled", "")
        .add_url_schemes(["file", "data"])
        // 既定では全 <a> に rel が付き、Markdown のリンクの出力まで変わる。
        .link_rel(None)
        .attribute_filter(move |element, attribute, value| {
            filter_attribute(element, attribute, value, &rewrite_local)
        });
    builder
}

/// ammonia の許可判定を生き延びた属性に対して、さらに絞り込みと書き換えを行う。
fn filter_attribute<'a>(
    element: &str,
    attribute: &str,
    value: &'a str,
    rewrite_local: &impl Fn(&str, &str, &str) -> Option<String>,
) -> Option<Cow<'a, str>> {
    match attribute {
        "class" => filter_classes(value),
        // URL を取る属性。`data:` は画像の `src` にだけ許す。
        "src" | "href" | "cite" => {
            if let Ok(url) = ammonia::Url::parse(value)
                && url.scheme() == "data"
            {
                return (element == "img" && attribute == "src" && is_data_image(&url))
                    .then_some(Cow::Borrowed(value));
            }
            if attribute == "cite" {
                return Some(Cow::Borrowed(value));
            }
            Some(match rewrite_local(element, attribute, value) {
                Some(rewritten) => Cow::Owned(rewritten),
                None => Cow::Borrowed(value),
            })
        }
        _ => Some(Cow::Borrowed(value)),
    }
}

/// comrak と syntect が出す class だけを残す。1 つも残らなければ属性ごと落とす。
///
/// ammonia の `allowed_classes` は class 属性の一般許可と併用できず、接頭辞でも
/// 指定できないので、ここで絞る。
fn filter_classes(value: &str) -> Option<Cow<'_, str>> {
    let kept: Vec<&str> = value
        .split_ascii_whitespace()
        .filter(|class| {
            COMRAK_CLASSES.contains(class)
                || class.starts_with(SYNTECT_CLASS_PREFIX)
                || class.starts_with(LANGUAGE_CLASS_PREFIX)
        })
        .collect();
    (!kept.is_empty()).then(|| Cow::Owned(kept.join(" ")))
}

fn is_data_image(url: &ammonia::Url) -> bool {
    let media_type = url.path().split([';', ',']).next().unwrap_or_default();
    DATA_IMAGE_TYPES
        .iter()
        .any(|allowed| media_type.trim().eq_ignore_ascii_case(allowed))
}

/// ammonia と同じ条件 (`<div>` の中身として、既定の `ParseOpts` で) 木にする。
///
/// ammonia は DOM を公開しないので、木は markup5ever_rcdom で作る。その html5ever は
/// ammonia のものと版が違いうる。どちらも HTML 仕様どおりの構文木を作るので形は一致する
/// 見込みだが、食い違う入力では警告と実際の除去がずれうる。
fn parse(html: &str) -> RcDom {
    parse_fragment(
        RcDom::default(),
        ParseOpts::default(),
        QualName::new(None, ns!(html), local_name!("div")),
        Vec::new(),
        false,
    )
    .one(html)
}

/// サニタイズ前後の木で、要素名と (要素名, 属性名) の出現数を比べて、取り除いたものを求める。
///
/// ammonia は何を取り除いたかを返さず、`attribute_filter` も許可判定を生き延びた
/// 属性にしか呼ばれない。判定を再実装すると警告と実際の除去が食い違いうるので、
/// 結果の差分から求める。
///
/// 返すのは、取り除いたタグ (`<tag>`) と、残ったタグから取り除いた属性 (`<tag attr>`)。
/// 重複を除いた出現順。
pub(crate) fn removed_markup(before: &str, after: &str) -> Vec<String> {
    let before_elements = elements(&parse(before));
    let after_elements = elements(&parse(after));
    let (before_tags, before_attributes) = count(&before_elements);
    let (after_tags, after_attributes) = count(&after_elements);

    let mut removed = Vec::new();
    let mut seen = HashSet::new();
    for (tag, attributes) in &before_elements {
        let kept = after_tags.get(tag.as_str()).copied().unwrap_or(0);
        let dropped = before_tags[tag.as_str()] - kept.min(before_tags[tag.as_str()]);
        if dropped > 0 && seen.insert((tag.as_str(), None)) {
            removed.push(format!("<{}>", printable(tag)));
        }
        // 取り除いたタグに付いていた属性は併記しない。同じ名前のタグが一部だけ
        // 取り除かれたときは、どの要素が消えたか分からないので、消えた要素の数を
        // 超えて減った属性だけを、残った要素から落ちたものとみなす。
        if kept == 0 {
            continue;
        }
        for attribute in attributes {
            let key = (tag.as_str(), attribute.as_str());
            let lost = before_attributes[&key]
                .saturating_sub(after_attributes.get(&key).copied().unwrap_or(0));
            if lost > dropped && seen.insert((tag.as_str(), Some(attribute.as_str()))) {
                removed.push(format!("<{} {}>", printable(tag), printable(attribute)));
            }
        }
    }
    removed
}

/// タグ名・属性名に入りうる制御文字を置き換える。警告は端末にも出るので、
/// エスケープシーケンスを通さない。
fn printable(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_control() { '\u{fffd}' } else { c })
        .collect()
}

type Counts<'a> = (HashMap<&'a str, usize>, HashMap<(&'a str, &'a str), usize>);

fn count(elements: &[(String, Vec<String>)]) -> Counts<'_> {
    let mut tags = HashMap::new();
    let mut attributes = HashMap::new();
    for (tag, names) in elements {
        *tags.entry(tag.as_str()).or_default() += 1;
        for name in names {
            *attributes.entry((tag.as_str(), name.as_str())).or_default() += 1;
        }
    }
    (tags, attributes)
}

/// 文書順の要素と、その属性名。深い入れ子でもスタックを溢れさせないよう、再帰しない。
///
/// `<template>` の中身は子として持たず、ammonia も辿らないので数えない。
fn elements(dom: &RcDom) -> Vec<(String, Vec<String>)> {
    let mut out = Vec::new();
    // parse_fragment の結果は、文書の直下に置かれた <html> の子として並ぶ。
    let mut stack: Vec<Handle> = dom
        .document
        .children
        .borrow()
        .iter()
        .flat_map(|root| root.children.borrow().clone())
        .rev()
        .collect();
    while let Some(node) = stack.pop() {
        if let NodeData::Element { name, attrs, .. } = &node.data {
            let attributes = attrs
                .borrow()
                .iter()
                .map(|attr| match &attr.name.prefix {
                    Some(prefix) => format!("{prefix}:{}", attr.name.local),
                    None => attr.name.local.to_string(),
                })
                .collect();
            out.push((name.local.to_string(), attributes));
        }
        stack.extend(node.children.borrow().iter().rev().cloned());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Sanitized {
        html: String,
        removed: Vec<String>,
    }

    fn clean(body: &str) -> Sanitized {
        let html = Sanitizer::new(|_, _, _| None).clean(body);
        let removed = removed_markup(body, &html);
        Sanitized { html, removed }
    }

    #[test]
    fn removed_tags_and_attributes_are_listed_in_order() {
        let sanitized = clean(
            "<script src=\"x.js\">alert(1)</script>\
             <img src=\"a.png\" onerror=\"alert(1)\">\
             <a href=\"javascript:alert(1)\">x</a>\
             <img src=\"b.png\" onerror=\"alert(2)\">",
        );
        assert_eq!(sanitized.removed, ["<script>", "<img onerror>", "<a href>"]);
        assert!(!sanitized.html.contains("alert"), "{}", sanitized.html);
    }

    #[test]
    fn nothing_is_listed_when_nothing_is_removed() {
        let sanitized = clean("<p>a<br>b</p><!-- comment -->");
        assert!(sanitized.removed.is_empty(), "{:?}", sanitized.removed);
    }

    #[test]
    fn inputs_are_fixed_to_disabled_checkboxes() {
        let sanitized = clean("<input type=\"text\" value=\"x\">");
        assert_eq!(
            sanitized.html, "<input type=\"checkbox\" disabled=\"\">",
            "{}",
            sanitized.html
        );
        assert_eq!(sanitized.removed, ["<input value>"]);
    }

    #[test]
    fn only_known_classes_survive() {
        let sanitized =
            clean("<span class=\"hl-source evil\">a</span><div class=\"mdopen-warnings\">b</div>");
        assert!(
            sanitized
                .html
                .contains("<span class=\"hl-source\">a</span>")
        );
        assert!(
            sanitized.html.contains("<div>b</div>"),
            "{}",
            sanitized.html
        );
        assert_eq!(sanitized.removed, ["<div class>"]);
    }

    #[test]
    fn data_urls_are_limited_to_raster_image_sources() {
        let sanitized = clean(
            "<img src=\"data:image/png;base64,AAAA\">\
             <img src=\"data:image/svg+xml,<svg/>\">\
             <a href=\"data:text/html,x\">x</a>\
             <a href=\"data:image/png;base64,AAAA\">y</a>\
             <q cite=\"data:text/html,x\">z</q>",
        );
        assert!(
            sanitized
                .html
                .contains("<img src=\"data:image/png;base64,AAAA\">")
        );
        assert_eq!(sanitized.removed, ["<img src>", "<a href>", "<q cite>"]);
        assert!(!sanitized.html.contains("<a href="), "{}", sanitized.html);
    }

    #[test]
    fn attributes_lost_on_kept_elements_are_reported_when_some_are_removed() {
        // svg の中の <a> は取り除かれ、残る <a> の href も落ちる。
        let sanitized =
            clean("<svg><a href=\"#x\">y</a></svg><a href=\"javascript:alert(1)\">z</a>");
        assert_eq!(sanitized.removed, ["<svg>", "<a>", "<a href>"]);
    }

    #[test]
    fn hidden_content_is_removed_with_its_element() {
        let sanitized =
            clean("<noscript><p>Enable JS</p></noscript><iframe src=\"x\"><b>f</b></iframe>");
        assert_eq!(sanitized.html, "", "{}", sanitized.html);
        assert_eq!(sanitized.removed, ["<noscript>", "<iframe>"]);
    }

    #[test]
    fn attributes_of_removed_elements_are_not_reported() {
        let sanitized = clean("<svg><a href=\"#x\">y</a></svg><a href=\"#z\">z</a>");
        assert_eq!(sanitized.removed, ["<svg>", "<a>"]);
    }

    #[test]
    fn all_raster_data_images_survive() {
        let body = "<img src=\"data:image/gif;base64,AAAA\">\
                    <img src=\"data:image/jpeg;base64,AAAA\">\
                    <img src=\"data:image/webp;base64,AAAA\">\
                    <img src=\"data:image/PNG;base64,AAAA\">";
        let sanitized = clean(body);
        assert_eq!(sanitized.html, body);
        assert!(sanitized.removed.is_empty(), "{:?}", sanitized.removed);
    }

    #[test]
    fn control_characters_in_names_are_not_reported_verbatim() {
        let sanitized = clean("<x\u{1b}]0;t\u{7} y>z</x\u{1b}]0;t\u{7}>");
        assert!(
            sanitized
                .removed
                .iter()
                .all(|r| !r.contains(char::is_control)),
            "{:?}",
            sanitized.removed
        );
        assert!(!sanitized.removed.is_empty());
    }

    #[test]
    fn misnested_markup_is_not_reported() {
        // 構文木の組み直し (adoption agency) で要素が複製されても、前後で同じに数える。
        let sanitized = clean("<b><i>x</b>y</i><table><tr><td>z</table>");
        assert!(sanitized.removed.is_empty(), "{:?}", sanitized.removed);
    }
}
