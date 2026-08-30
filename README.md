# mdo

[日本語](README.ja.md)

Convert a Markdown file into a single HTML page and open it in the default browser.

```
mdo file.md
```

Handing an md file to `mdo.app` from Finder does the same thing.

## Limitations

All of these are deliberate choices, and all of them are surprising if you run into them unaware.

- Raw HTML is not included in the output; `<details>` and `<br>` are dropped along with their tags. The converted page is opened over `file://`, so letting a `<script>` or an `<img onerror>` written in the Markdown through would run it in a context that can read local files. When something is dropped, a warning appears at the top of the page
- Opening a document that contains a mermaid diagram fetches the rendering library from jsdelivr. The fetched content is pinned with SRI, but the connection itself does happen. A document without diagrams loads no JavaScript at all
- Images are referenced rather than embedded. Moving or deleting the original file breaks the page as well
- Output is never cleaned up. It stays under `$TMPDIR/mdo/`, readable only by its owner

## License

Dual-licensed under MIT or Apache-2.0.
