# WS5: GitHub Pages Website — Spec

**Status:** In Progress

## Planned Changes

### Landing page (`website/`)
- `index.html` — hero, how it works, features, security, install, comparison
- `style.css` — dark terminal aesthetic, responsive, system fonts

### mdBook documentation (`book/`)
- `book.toml` — mdBook config
- `book/src/SUMMARY.md` — chapter list
- `book/src/*.md` — adapted from docs/*.md

### Deployment (`.github/workflows/pages.yml`)
- Build mdBook, assemble site, deploy with actions/deploy-pages@v4

## Verification

Push to main, site deploys, all pages render correctly.
