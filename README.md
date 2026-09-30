# mdopen

[日本語](README.ja.md)

Convert a Markdown file into a single HTML page and print where it went.

```
mdhtml file.md
```

Handing an md file to `mdopen.app` from Finder converts it and opens it in the default browser.

## Install

Install the `mdhtml` command:

```
brew install gin0606/tap/mdhtml
```

`mdopen.app` is only needed to open files from Finder, and comes from a separate cask:

```
brew install --cask gin0606/tap/mdopen
```

`mdopen.app` does not make itself the default for Markdown files, so macOS keeps whatever it was already opening them with. Use Open With, or drop the file onto the app.

## Limitations

All of these are deliberate choices, and all of them are surprising if you run into them unaware.

- Raw HTML passes through an allowlist. Harmless tags such as `<details>`, `<br>` and `<img>` are kept, while `<script>`, `<iframe>`, `on*` attributes and `style` attributes are removed. The converted page is opened over `file://`, so letting them through would run scripts in a context that can read local files. Removed tags and attributes are listed in a warning at the top of the page. Raw HTML left unclosed, such as a bare `<script>` mentioned in a sentence, is shown as text when it would otherwise delete the rest of the document. An unclosed tag that only changes formatting, such as `<code>`, still carries its style over into what follows
- Link and image URLs are limited to the common schemes (http, https, mailto, file and the like). Links with custom schemes such as `obsidian://` or `vscode://` lose their target, even when written in Markdown. `data:` URLs are allowed only for PNG, GIF, JPEG and WebP images
- Opening a document that contains a mermaid diagram fetches the rendering library from jsdelivr. The fetched content is pinned with SRI, but the connection itself does happen. A document without diagrams loads no JavaScript at all
- The converted page carries a Content Security Policy that lets only the mermaid library and its startup script run (none at all in a document without diagrams). Even if a script slipped into the output, the browser would refuse to run it
- Images are referenced rather than embedded. Moving or deleting the original file breaks the page as well
- Output is never cleaned up. It stays under `$TMPDIR/mdopen/`, readable only by its owner

## License

Dual-licensed under MIT or Apache-2.0.
