# Corpus sources

Real public pages, captured once with `pageon-bench capture` on 2026-11-12. They contain
third-party content, so they are **not committed**: `real-*` captures and their snapshots are
gitignored. Recreate them locally from `bench/`:

```sh
uv run pageon-bench capture <url> -o ../crates/pageon-agent/tests/corpus/real-<name>.capture.json.gz
INSTA_UPDATE=always cargo test -p pageon-agent --test golden   # writes their local snapshots
```

Tests and reports use the real pages when they are present (golden snapshots, the release
timing test, real-page token sizes). CI runs on the constructed corpus alone.

Real pages last captured: The tool checks
robots.txt and identifies itself with a `pageon-bench/<version>` UA token. Each page was loaded
once, without credentials. en.wikipedia.org was skipped because its robots.txt disallows the
bench's UA token.

| Capture | URL |
| --- | --- |
| real-mdn | https://developer.mozilla.org/en-US/docs/Web/HTML/Reference/Elements/form |
| real-github | https://github.com/hupe1980/procage |
| real-python-docs | https://docs.python.org/3/library/json.html |
| real-hackernews | https://news.ycombinator.com/ |
| real-govuk | https://www.gov.uk/ |
| real-shop | https://books.toscrape.com/ |
| real-form | https://httpbin.org/forms/post |
| real-rust-lang | https://www.rust-lang.org/ |
| real-crates-search | https://crates.io/search?q=html5ever |
| real-todomvc | https://todomvc.com/examples/react/dist/ |
| real-bbc-news | https://www.bbc.com/news |

The constructed captures (`*.capture.json.gz` without the `real-` prefix, and `sequences/`) are
rebuilt from `bench/fixtures/` with `uv run pageon-bench corpus`.
