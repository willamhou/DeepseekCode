# Web Search Bing Default

## Context

DeepSeek-TUI changed `web_search` to use Bing HTML search by default while
keeping DuckDuckGo selectable. DeepSeekCode still defaulted to DuckDuckGo HTML
search, with only `DSCODE_WEB_SEARCH_URL_TEMPLATE` available for deterministic
tests and custom gateways.

## Spec

- Change the default `web_search` URL to `https://www.bing.com/search`.
- Report `meta.source=bing` when no custom URL template is configured.
- Keep DuckDuckGo available through `DSCODE_WEB_SEARCH_PROVIDER=duckduckgo`
  or `ddg`.
- Preserve `DSCODE_WEB_SEARCH_URL_TEMPLATE` precedence for tests and private
  gateways.
- Update runtime and parity docs so they no longer claim DuckDuckGo is the
  default text search backend.

## Verification

- `web_search_defaults_to_bing_url_and_source`
- `web_search_provider_env_keeps_duckduckgo_available`
- `cargo test web_search --lib`
- `cargo check`
- `cargo test --lib -- --test-threads=1`
- `git diff --check`
