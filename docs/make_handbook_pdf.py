#!/usr/bin/env python3
"""Set docs/HANDBOOK.md as a quiet A5 booklet, docs/handbook.pdf.

The markdown stays the manuscript. This only sets the type: a Palatino
page, wide margins, and the source notes as footnotes. Run it again
after the handbook changes.

Looks for `typst` on PATH, then docs/.cache/typst. If neither is there,
downloads the pinned linux binary into the cache (gitignored).
"""

from __future__ import annotations

import re
import shutil
import subprocess
import tarfile
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
SRC = ROOT / "docs" / "HANDBOOK.md"
OUT = ROOT / "docs" / "handbook.pdf"
CACHE = ROOT / "docs" / ".cache"
TYPST_VERSION = "0.15.1"
TYPST_URL = (
    "https://github.com/typst/typst/releases/download/"
    f"v{TYPST_VERSION}/typst-x86_64-unknown-linux-musl.tar.xz"
)
FONT_PATH = Path("/usr/share/fonts/opentype/urw-base35")

CLUSTER = re.compile(
    r"[ \t]*\(((?:\s*\[[^\]]+\]\([^)]+\)\s*;)*\s*\[[^\]]+\]\([^)]+\)\s*)\)",
    re.S,
)
LINK = re.compile(r"\[([^\]]+)\]\(([^)]+)\)")
HEADING = re.compile(r"^(\d+)\.\s+(.*)$")


def typst_bin() -> Path:
    found = shutil.which("typst")
    if found:
        return Path(found)
    dest = CACHE / "typst"
    if dest.is_file():
        return dest
    CACHE.mkdir(parents=True, exist_ok=True)
    archive = CACHE / "typst.tar.xz"
    urllib.request.urlretrieve(TYPST_URL, archive)
    with tarfile.open(archive) as tar:
        member = next(m for m in tar.getmembers() if m.name.endswith("/typst"))
        extracted = tar.extractfile(member)
        if extracted is None:
            raise SystemExit("typst archive has no binary")
        dest.write_bytes(extracted.read())
    dest.chmod(0o755)
    archive.unlink(missing_ok=True)
    return dest


def esc(text: str) -> str:
    """Escape a run of prose for Typst markup, and curl apostrophes."""
    text = text.replace("\\", "\\\\")
    for ch in "#$*_`[]<>@":
        text = text.replace(ch, "\\" + ch)
    return text.replace("'", "’")


def esc_string(text: str) -> str:
    return text.replace("\\", "\\\\").replace('"', '\\"')


def breakable(url: str) -> str:
    shown = esc(url)
    for ch in "/-?&=":
        shown = shown.replace(ch, ch + "\u200b")
    return shown


def source_line(label: str, url: str) -> str:
    if url.endswith("DETAILED.md"):
        return "The detailed manual, #emph[DETAILED.md], in the same project."
    return f'{esc(label)}. #link("{esc_string(url)}")[{breakable(url)}]'


def footnote(label: str, url: str) -> str:
    return "#footnote[" + source_line(label, url) + "]"


def stash_sources(text: str, notes: list[str]) -> str:
    """One note per citation cluster, so the marks do not run together."""

    def repl(match: re.Match[str]) -> str:
        parts = LINK.findall(match.group(1))
        body = " #linebreak()".join(source_line(label, url) for label, url in parts)
        notes.append("#footnote[" + body + "]")
        return f"\x00{len(notes) - 1}\x00"

    return CLUSTER.sub(repl, text)


def emit_plain(chunk: str, notes: list[str]) -> str:
    pieces = re.split(r"\x00(\d+)\x00", chunk)
    out = []
    for i, piece in enumerate(pieces):
        if i % 2 == 0:
            out.append(esc(piece))
        else:
            out.append(notes[int(piece)])
    return "".join(out)


def parse_inline(text: str, notes: list[str]) -> str:
    text = stash_sources(text, notes)
    out: list[str] = []
    i = 0
    n = len(text)
    while i < n:
        if text.startswith("**", i):
            j = text.find("**", i + 2)
            if j == -1:
                out.append(emit_plain(text[i:], notes))
                break
            out.append("#strong[" + esc(text[i + 2 : j]) + "]")
            i = j + 2
            continue
        if text.startswith("`", i):
            j = text.find("`", i + 1)
            if j == -1:
                out.append(emit_plain(text[i:], notes))
                break
            out.append(esc(text[i + 1 : j]))
            i = j + 1
            continue
        if text[i] == "*":
            j = text.find("*", i + 1)
            if j == -1:
                out.append(emit_plain(text[i:], notes))
                break
            out.append("#emph[" + esc(text[i + 1 : j]) + "]")
            i = j + 1
            continue
        if text[i] == "[":
            matched = LINK.match(text, i)
            if matched:
                label, url = matched.group(1), matched.group(2)
                out.append(esc(label) + footnote(label, url))
                i = matched.end()
                continue
        j = i + 1
        while j < n and text[j] not in "*`[":
            j += 1
        out.append(emit_plain(text[i:j], notes))
        i = j
    return "".join(out)


def blocks(markdown: str) -> list[tuple[str, str]]:
    items: list[tuple[str, str]] = []
    for raw in re.split(r"\n\s*\n", markdown.strip()):
        lines = [line.strip() for line in raw.splitlines() if line.strip()]
        if not lines or lines == ["---"]:
            continue
        if lines[0].startswith("# "):
            items.append(("title", lines[0][2:].strip()))
            continue
        if lines[0].startswith("## "):
            items.append(("h", lines[0][3:].strip()))
            rest = " ".join(lines[1:])
            if rest:
                items.append(("p", rest))
            continue
        items.append(("p", " ".join(lines)))
    return items


def chapter_call(heading: str) -> str:
    matched = HEADING.match(heading)
    title = matched.group(2) if matched else heading
    return (
        "#chapter[\n"
        f'  #text(size: 16pt, weight: "regular")[{esc(title)}]\n'
        "]"
    )


PREAMBLE = r'''#set document(title: "Mimizan Lab — Handbook", author: "Dragan Bojovic")
#set text(font: "P052", size: 11pt, fill: rgb("#1a1a1a"), lang: "en", region: "GB", hyphenate: true)
#set par(leading: 0.58em, spacing: 0.95em, justify: true, linebreaks: "optimized", first-line-indent: 0pt)
#set page(
  paper: "a5",
  margin: (x: 20mm, top: 22mm, bottom: 18mm),
  numbering: none,
  footer: none,
)
// Keep a few words whole. The hyphenator breaks "histogram" as his-togram.
#show "histogram": it => box(it)

#show footnote.entry: it => context {
  set text(size: 8pt, fill: rgb("#3a3a3a"), hyphenate: false)
  set par(justify: false, leading: 0.32em, spacing: 0.28em, first-line-indent: 0pt, hanging-indent: 1.35em)
  let num = counter(footnote).at(it.note.location()).first()
  let on-page = query(footnote).filter(f => f.location().page() == it.note.location().page())
  let first = on-page.len() > 0 and counter(footnote).at(on-page.first().location()).first() == num
  block(above: 0.15em, below: 0.12em)[
    #if first [
      #line(length: 22mm, stroke: 0.4pt + rgb("#b5b5b5"))
      #v(0.55em)
    ]
    #super[#num]#h(0.45em)#it.note.body
  ]
}

#let chapter(body) = {
  pagebreak(weak: true)
  block(breakable: false, above: 0pt, below: 7mm, body)
}

#align(center)[
  #v(34mm)
  #text(size: 9pt, tracking: 0.32em, fill: rgb("#3a3a3a"))[MIMIZAN LAB]
  #v(5.5mm)
  #line(length: 28mm, stroke: 0.45pt + rgb("#b5b5b5"))
  #v(5.5mm)
  #text(size: 22pt, weight: "regular")[Handbook]
]

#v(18mm)
'''

BODY_PAGE = r'''
#pagebreak()
#set page(footer: context {
  align(center, text(size: 9pt, fill: rgb("#6a6a6a"), counter(page).display("1")))
})
#counter(page).update(1)
'''


def build_typst(markdown: str) -> str:
    notes: list[str] = []
    parsed = blocks(markdown)
    prelude: list[str] = []
    body: list[str] = []
    seen_chapter = False
    for kind, text in parsed:
        if kind == "title":
            continue
        if kind == "h":
            seen_chapter = True
            body.append(chapter_call(text))
            continue
        rendered = parse_inline(text, notes)
        if seen_chapter:
            body.append(rendered)
        else:
            prelude.append(rendered)
    parts = [PREAMBLE, "\n\n".join(prelude), BODY_PAGE, "\n\n".join(body), ""]
    return "\n".join(parts)


def main() -> None:
    markdown = SRC.read_text(encoding="utf-8")
    typst = CACHE / "handbook.typ"
    CACHE.mkdir(parents=True, exist_ok=True)
    typst.write_text(build_typst(markdown), encoding="utf-8")
    cmd = [str(typst_bin()), "compile", "--root", str(ROOT)]
    if FONT_PATH.is_dir():
        cmd += ["--font-path", str(FONT_PATH)]
    cmd += [str(typst), str(OUT)]
    subprocess.run(cmd, check=True)
    print(OUT)


if __name__ == "__main__":
    main()
