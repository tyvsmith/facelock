#!/usr/bin/env python3
"""Validate local links, fragments and assets in the actual assembled HTML site."""
from __future__ import annotations

import argparse
from html.parser import HTMLParser
from pathlib import Path
from urllib.parse import unquote, urlsplit


class Page(HTMLParser):
    def __init__(self):
        super().__init__(convert_charrefs=True)
        self.ids = set()
        self.links = []

    def handle_starttag(self, tag, attrs):
        attrs = dict(attrs)
        if attrs.get("id"):
            self.ids.add(attrs["id"])
        if tag == "a" and attrs.get("name"):
            self.ids.add(attrs["name"])
        for attribute in ("href", "src"):
            if attrs.get(attribute):
                self.links.append((self.getpos()[0], attrs[attribute]))


def check_site(root):
    root = root.resolve()
    pages = {}
    errors = []
    for path in sorted(root.rglob("*.html")):
        parser = Page()
        parser.feed(path.read_text())
        pages[path.resolve()] = parser
    if not pages or not (root / "index.html").is_file():
        return ["site must contain an index.html and rendered pages"]
    for path, page in pages.items():
        for line, value in page.links:
            url = urlsplit(value)
            if url.scheme or url.netloc:
                continue
            relative = unquote(url.path)
            destination = ((root / relative.lstrip("/")) if relative.startswith("/") else (path.parent / relative) if relative else path).resolve()
            context = f"{path.relative_to(root)}:{line}: {value}"
            if not destination.is_relative_to(root):
                errors.append(f"{context}: link escapes assembled site")
                continue
            if destination.is_dir():
                destination /= "index.html"
            if not destination.is_file():
                errors.append(f"{context}: missing file")
            elif url.fragment and destination in pages and unquote(url.fragment) not in pages[destination].ids:
                errors.append(f"{context}: missing fragment")
    return errors


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("site", type=Path)
    args = parser.parse_args()
    errors = check_site(args.site)
    for error in errors:
        print(error)
    if not errors:
        print("rendered documentation links and assets verified")
    return bool(errors)


if __name__ == "__main__":
    raise SystemExit(main())
