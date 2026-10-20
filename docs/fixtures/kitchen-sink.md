title:: kitchen-sink
tags:: fixture, qa

- Every block below names what it should look like. Read each one in view mode, then click it and check the editor shows the raw markdown. See docs/fixtures/README.md for the steps.
- **Bold**, *italic*, ***bold italic***, ~~struck through~~ and `inline code`.
- Styles nest: **bold with *italic* inside** and *italic with `code` inside*.
- An http link opens the browser: [Logseq](https://logseq.com). A non-http link does nothing on click: [relative](notes/elsewhere.md).
- Page links are blue with faint brackets: [[kitchen-sink]] opens this page, and so does [[Kitchen-Sink]], because names are case-insensitive.
- A date link opens that day's journal, not a page: [[2026-07-13]].
- #foo is plain text, not a tag.
- Inline HTML shows as text: <b>not bold</b>.
- # Heading 1, with a rule beneath
- ## Heading 2, with a rule beneath
- ### Heading 3, no rule
- > A blockquote: muted text with a bar on the left.
  >
  > > A nested quote, indented again.
- Rust, highlighted:
  ```rust
  // A comment is italic.
  fn main() {
      let n = 42;
      println!("{n}");
  }
  ```
- Python, highlighted:
  ```python
  def greet(name: str) -> None:
      print(f"hello {name}")
  ```
- An unknown language is plain monospace:
  ```nosuchlang
  stays = uncoloured
  ```
- ```calc
  # Each line's result shows beside it; nothing is written back to the file.
  price = 12.50
  qty = 4
  price * qty
  1 / 0
  ```
- A block with continuation lines.
  This line wraps into the same paragraph (a soft break).\
  This line starts after a hard break.

  This is a second paragraph after a blank line.
- A long paragraph has to wrap at the window edge rather than run off it or collapse into a narrow column. Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor incididunt ut labore et dolore magna aliqua, with **bold**, *italic*, `code` and a [[kitchen-sink]] link along the way.
- Unicode: café, naïve, 日本語, emoji 🎉.
-
- The bullet above is an empty block, saved as a bare `-`.
- Parent
  - Child
    - Grandchild
  - Second child
