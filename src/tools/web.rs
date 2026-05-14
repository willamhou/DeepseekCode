use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::process::Command;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::config::types::NetworkConfig;
use crate::core::network_policy::{audit_decision, decide, NetworkDecision};
use crate::error::{app_error, AppResult};
use crate::tools::types::{Tool, ToolInput, ToolOutput};
use crate::util::json::{json_value_to_string, parse_json_value, JsonValue};

const DEFAULT_TIMEOUT_MS: u64 = 15_000;
const HARD_TIMEOUT_MS: u64 = 60_000;
const DEFAULT_FETCH_MAX_BYTES: usize = 1_000_000;
const HARD_FETCH_MAX_BYTES: usize = 10_485_760;
const DEFAULT_SEARCH_MAX_RESULTS: usize = 5;
const HARD_SEARCH_MAX_RESULTS: usize = 10;
const SEARCH_FETCH_MAX_BYTES: usize = 2_000_000;
const USER_AGENT: &str = "DeepSeekCode/0.1 (+https://github.com/willamhou/DeepSeekCode)";
const CURL_META_MARKER: &str = "\n__DSCODE_FETCH_META__";
const PDF_TEXT_COMMAND_ENV: &str = "DSCODE_PDF_TEXT_COMMAND";
pub(crate) const NETWORK_APPROVED_ARG: &str = "__dscode_network_approved";

static WEB_RUN_STATE: OnceLock<Mutex<WebRunState>> = OnceLock::new();

pub struct FetchUrlTool;

impl Tool for FetchUrlTool {
    fn name(&self) -> &str {
        "fetch_url"
    }

    fn execute(&self, input: ToolInput) -> AppResult<ToolOutput> {
        let url = required_trimmed(&input, "url", "fetch_url requires `url`")?;
        let view = fetch_page_view_from_input(url, &input, None)?;
        Ok(ToolOutput {
            summary: view.summary,
        })
    }
}

pub struct WebSearchTool;

impl Tool for WebSearchTool {
    fn name(&self) -> &str {
        "web_search"
    }

    fn execute(&self, input: ToolInput) -> AppResult<ToolOutput> {
        let request = search_request(&input)?;
        if request.query.trim().is_empty() {
            return Err(app_error("web_search query cannot be empty"));
        }

        let url = search_url(&request.query);
        let fetched = fetch_url(
            &url,
            SEARCH_FETCH_MAX_BYTES,
            request.timeout_ms.clamp(1, HARD_TIMEOUT_MS),
            network_approved(&input),
        )?;
        let results = parse_search_results(&fetched.body, request.max_results);

        let mut summary = String::new();
        summary.push_str(&format!("meta.query={}\n", sanitize_meta(&request.query)));
        summary.push_str(&format!(
            "meta.source={}\n",
            if search_template().is_some() {
                "custom"
            } else {
                "duckduckgo"
            }
        ));
        summary.push_str(&format!("meta.count={}\n", results.len()));
        if results.is_empty() {
            summary.push_str("No results found.\n");
            return Ok(ToolOutput { summary });
        }

        summary.push_str(&format!("Found {} result(s):\n", results.len()));
        for (index, result) in results.iter().enumerate() {
            summary.push_str(&format!(
                "[{}] {}\nurl: {}\nref_id: search{}\n",
                index + 1,
                result.title,
                result.url,
                index
            ));
            if !result.snippet.is_empty() {
                summary.push_str(&format!("snippet: {}\n", result.snippet));
            }
        }
        Ok(ToolOutput { summary })
    }
}

pub struct FinanceTool;

impl Tool for FinanceTool {
    fn name(&self) -> &str {
        "finance"
    }

    fn execute(&self, input: ToolInput) -> AppResult<ToolOutput> {
        let request = finance_request(&input)?;
        let url = finance_url(&request.ticker);
        let fetched = fetch_url(
            &url,
            DEFAULT_FETCH_MAX_BYTES,
            request.timeout_ms.clamp(1, HARD_TIMEOUT_MS),
            network_approved(&input),
        )?;
        let quote = parse_finance_quote(&fetched.body, &request.ticker)?;

        let mut summary = String::new();
        summary.push_str(&format!("meta.ticker={}\n", sanitize_meta(&request.ticker)));
        summary.push_str(&format!(
            "meta.source={}\n",
            if finance_template().is_some() {
                "custom"
            } else {
                "yahoo-finance"
            }
        ));
        summary.push_str(&format!("symbol: {}\n", quote.symbol));
        if let Some(name) = quote.name {
            summary.push_str(&format!("name: {name}\n"));
        }
        if let Some(price) = quote.price {
            summary.push_str("price: ");
            summary.push_str(&price);
            if let Some(currency) = quote.currency.as_deref() {
                summary.push(' ');
                summary.push_str(currency);
            }
            summary.push('\n');
        }
        if quote.change.is_some() || quote.change_percent.is_some() {
            summary.push_str("change: ");
            summary.push_str(quote.change.as_deref().unwrap_or("n/a"));
            if let Some(percent) = quote.change_percent {
                summary.push_str(&format!(" ({percent}%)"));
            }
            summary.push('\n');
        }
        if let Some(exchange) = quote.exchange {
            summary.push_str(&format!("exchange: {exchange}\n"));
        }
        if let Some(market_state) = quote.market_state {
            summary.push_str(&format!("market_state: {market_state}\n"));
        }
        Ok(ToolOutput { summary })
    }
}

pub struct WebRunTool;

impl Tool for WebRunTool {
    fn name(&self) -> &str {
        "web_run"
    }

    fn execute(&self, input: ToolInput) -> AppResult<ToolOutput> {
        let turn_id = next_web_run_turn_id()?;
        let response_length = WebRunResponseLength::from_arg(input.get("response_length"));
        let network_approved = network_approved(&input);
        let mut summary = format!("meta.tool=web_run\nmeta.turn_id={turn_id}\n");
        let mut supported_actions = 0usize;
        let mut unsupported_actions = Vec::new();
        let mut search_ref_index = 0usize;

        if let Some(raw) = input.get("search_query") {
            for (index, action) in web_run_action_objects("search_query", raw)?
                .iter()
                .enumerate()
            {
                let mut search_input = tool_input_from_json_object(action);
                mark_network_approved(&mut search_input, network_approved);
                let mut output = WebSearchTool.execute(search_input)?;
                let refs = store_web_run_search_refs(turn_id, &output.summary, search_ref_index)?;
                search_ref_index += refs.len();
                append_ref_lines(&mut output.summary, &refs);
                append_web_run_result(&mut summary, "search_query", index, output)?;
                supported_actions += 1;
            }
        }

        if let Some(raw) = input.get("open") {
            for (index, action) in web_run_action_objects("open", raw)?.iter().enumerate() {
                let mut fetch_input = tool_input_from_json_object(action);
                let resolved = web_run_resolve_url(action, "open")?;
                fetch_input
                    .args
                    .insert("url".to_string(), resolved.url.clone());
                mark_network_approved(&mut fetch_input, network_approved);
                let view_options = WebRunViewOptions::from_action(action, response_length);
                let view =
                    fetch_page_view_from_input(&resolved.url, &fetch_input, Some(view_options))?;
                let mut output = ToolOutput {
                    summary: view.summary,
                };
                let refs = store_web_run_open_page(
                    turn_id,
                    index,
                    resolved.source_ref.as_deref(),
                    &resolved.url,
                    view.cache,
                )?;
                append_ref_lines(&mut output.summary, &refs);
                append_web_run_result(&mut summary, "open", index, output)?;
                supported_actions += 1;
            }
        }

        if let Some(raw) = input.get("click") {
            for (index, action) in web_run_action_objects("click", raw)?.iter().enumerate() {
                let output =
                    web_run_click(turn_id, index, action, response_length, network_approved)?;
                append_web_run_result(&mut summary, "click", index, output)?;
                supported_actions += 1;
            }
        }

        if let Some(raw) = input.get("find") {
            for (index, action) in web_run_action_objects("find", raw)?.iter().enumerate() {
                let output = web_run_find(action, network_approved)?;
                append_web_run_result(&mut summary, "find", index, output)?;
                supported_actions += 1;
            }
        }

        if let Some(raw) = input.get("finance") {
            for (index, action) in web_run_action_objects("finance", raw)?.iter().enumerate() {
                let mut finance_input = tool_input_from_json_object(action);
                mark_network_approved(&mut finance_input, network_approved);
                let output = FinanceTool.execute(finance_input);
                append_web_run_result(&mut summary, "finance", index, output?)?;
                supported_actions += 1;
            }
        }

        if let Some(raw) = input.get("image_query") {
            for (index, action) in web_run_action_objects("image_query", raw)?
                .iter()
                .enumerate()
            {
                let output = web_run_image_query(action, network_approved)?;
                append_web_run_result(&mut summary, "image_query", index, output)?;
                supported_actions += 1;
            }
        }

        if let Some(raw) = input.get("screenshot") {
            for (index, action) in web_run_action_objects("screenshot", raw)?
                .iter()
                .enumerate()
            {
                let output = web_run_screenshot(action)?;
                append_web_run_result(&mut summary, "screenshot", index, output)?;
                supported_actions += 1;
            }
        }

        for action in ["weather", "sports", "time"] {
            if input.get(action).is_some() {
                unsupported_actions.push(action);
            }
        }

        if supported_actions == 0 && unsupported_actions.is_empty() {
            return Err(app_error(
                "web_run requires one of search_query, open, click, find, finance, image_query, or screenshot",
            ));
        }

        summary.insert_str(
            web_run_meta_insert_index(&summary),
            &format!("meta.supported_actions={supported_actions}\n"),
        );
        if !unsupported_actions.is_empty() {
            summary.insert_str(
                web_run_meta_insert_index(&summary),
                &format!(
                    "meta.unsupported_actions={}\n",
                    unsupported_actions.join(",")
                ),
            );
            summary.push_str("Unsupported web_run actions in this slice: ");
            summary.push_str(&unsupported_actions.join(", "));
            summary.push('\n');
        }

        Ok(ToolOutput { summary })
    }
}

#[derive(Debug, Clone)]
struct FetchedUrl {
    final_url: String,
    status: u16,
    content_type: String,
    body: String,
    body_bytes: Vec<u8>,
    total_bytes: usize,
    truncated: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct FetchedUrlBytes {
    pub final_url: String,
    pub status: u16,
    pub body_bytes: Vec<u8>,
    pub total_bytes: usize,
    pub truncated: bool,
}

#[derive(Debug, Clone)]
struct FetchedPageView {
    summary: String,
    cache: WebRunCachedPage,
}

#[derive(Debug, Clone, Copy)]
struct WebRunViewOptions {
    lineno: usize,
    response_length: WebRunResponseLength,
}

impl WebRunViewOptions {
    fn from_action(
        map: &BTreeMap<String, JsonValue>,
        response_length: WebRunResponseLength,
    ) -> Self {
        Self {
            lineno: json_usize(map, "lineno").unwrap_or(1).max(1),
            response_length,
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum WebRunResponseLength {
    Short,
    Medium,
    Long,
}

impl WebRunResponseLength {
    fn from_arg(value: Option<&str>) -> Self {
        match value
            .map(str::trim)
            .unwrap_or("medium")
            .to_ascii_lowercase()
            .as_str()
        {
            "short" => Self::Short,
            "long" => Self::Long,
            _ => Self::Medium,
        }
    }

    fn view_lines(self) -> usize {
        match self {
            Self::Short => 40,
            Self::Medium => 80,
            Self::Long => 160,
        }
    }
}

#[derive(Debug, Clone)]
struct ParsedUrl {
    scheme: String,
    host: String,
    port: u16,
    path_query: String,
}

#[derive(Debug, Clone)]
struct SearchRequest {
    query: String,
    max_results: usize,
    timeout_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SearchResult {
    title: String,
    url: String,
    snippet: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct WebRunLink {
    id: usize,
    url: String,
    text: String,
}

#[derive(Debug, Clone, Default)]
struct WebRunCachedPage {
    text: String,
    links: Vec<WebRunLink>,
    pdf_pages: Vec<Vec<String>>,
}

#[derive(Debug, Clone)]
struct FinanceRequest {
    ticker: String,
    timeout_ms: u64,
}

#[derive(Debug, Clone)]
struct ImageQueryRequest {
    query: String,
    max_results: usize,
    timeout_ms: u64,
    domains: Vec<String>,
    recency: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ImageQueryResult {
    image: String,
    thumbnail: Option<String>,
    title: Option<String>,
    url: Option<String>,
    source: Option<String>,
    width: Option<String>,
    height: Option<String>,
}

#[derive(Debug, Clone)]
struct FinanceQuote {
    symbol: String,
    name: Option<String>,
    price: Option<String>,
    change: Option<String>,
    change_percent: Option<String>,
    currency: Option<String>,
    exchange: Option<String>,
    market_state: Option<String>,
}

#[derive(Debug, Default)]
struct WebRunState {
    next_turn: u64,
    urls: BTreeMap<String, String>,
    pages: BTreeMap<String, WebRunCachedPage>,
}

fn web_run_action_objects(action: &str, raw: &str) -> AppResult<Vec<BTreeMap<String, JsonValue>>> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }
    if action == "open" && is_direct_http_url(trimmed) {
        let mut map = BTreeMap::new();
        map.insert("ref_id".to_string(), JsonValue::String(trimmed.to_string()));
        return Ok(vec![map]);
    }

    let value = parse_json_value(trimmed)?;
    match value {
        JsonValue::Array(items) => items
            .into_iter()
            .map(|item| match item {
                JsonValue::Object(map) => Ok(map),
                JsonValue::String(value) if action == "open" && is_direct_http_url(&value) => {
                    let mut map = BTreeMap::new();
                    map.insert("ref_id".to_string(), JsonValue::String(value));
                    Ok(map)
                }
                _ => Err(app_error(format!(
                    "web_run `{action}` entries must be objects"
                ))),
            })
            .collect(),
        JsonValue::Object(map) => Ok(vec![map]),
        JsonValue::String(value) if action == "open" && is_direct_http_url(&value) => {
            let mut map = BTreeMap::new();
            map.insert("ref_id".to_string(), JsonValue::String(value));
            Ok(vec![map])
        }
        _ => Err(app_error(format!(
            "web_run `{action}` must be a JSON object or array"
        ))),
    }
}

fn tool_input_from_json_object(map: &BTreeMap<String, JsonValue>) -> ToolInput {
    let mut input = ToolInput::new();
    for (key, value) in map {
        let rendered = match value {
            JsonValue::String(value) => value.clone(),
            JsonValue::Number(value) => value.clone(),
            JsonValue::Bool(value) => value.to_string(),
            JsonValue::Null => "null".to_string(),
            JsonValue::Array(_) | JsonValue::Object(_) => json_value_to_string(value),
        };
        input.args.insert(key.clone(), rendered);
    }
    input
}

fn mark_network_approved(input: &mut ToolInput, approved: bool) {
    if approved {
        input
            .args
            .insert(NETWORK_APPROVED_ARG.to_string(), "true".to_string());
    }
}

fn network_approved(input: &ToolInput) -> bool {
    input
        .get(NETWORK_APPROVED_ARG)
        .map(|value| matches!(value, "1" | "true" | "TRUE" | "yes" | "on"))
        .unwrap_or(false)
}

#[derive(Debug, Clone)]
struct ResolvedWebRunUrl {
    url: String,
    source_ref: Option<String>,
}

fn web_run_raw_ref<'a>(map: &'a BTreeMap<String, JsonValue>, action: &str) -> AppResult<&'a str> {
    json_string(map, "url")
        .or_else(|| json_string(map, "ref_id"))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| app_error(format!("web_run `{action}` requires `url` or `ref_id`")))
}

fn web_run_resolve_url(
    map: &BTreeMap<String, JsonValue>,
    action: &str,
) -> AppResult<ResolvedWebRunUrl> {
    let raw = web_run_raw_ref(map, action)?;
    if is_direct_http_url(raw) {
        return Ok(ResolvedWebRunUrl {
            url: raw.to_string(),
            source_ref: None,
        });
    }
    if let Some(url) = lookup_web_run_url(raw)? {
        return Ok(ResolvedWebRunUrl {
            url,
            source_ref: Some(raw.to_string()),
        });
    }
    Err(app_error(format!(
        "web_run `{action}` could not resolve ref_id `{raw}`; use a direct URL or a ref from an earlier web_run search/open"
    )))
}

fn web_run_find(
    map: &BTreeMap<String, JsonValue>,
    network_approved: bool,
) -> AppResult<ToolOutput> {
    let raw_ref = web_run_raw_ref(map, "find")?.to_string();
    let pattern = json_string(map, "pattern")
        .or_else(|| json_string(map, "text"))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| app_error("web_run `find` requires `pattern`"))?;
    let max_bytes = json_usize(map, "max_bytes")
        .unwrap_or(DEFAULT_FETCH_MAX_BYTES)
        .clamp(1, HARD_FETCH_MAX_BYTES);
    let timeout_ms = json_u64(map, "timeout_ms")
        .unwrap_or(DEFAULT_TIMEOUT_MS)
        .clamp(1, HARD_TIMEOUT_MS);
    let max_matches = json_usize(map, "max_matches").unwrap_or(5).clamp(1, 20);

    let (url, text, source) = if let Some(cached) = lookup_web_run_page(&raw_ref)? {
        let url = lookup_web_run_url(&raw_ref)?.unwrap_or_else(|| raw_ref.clone());
        (url, cached.text, "cache")
    } else {
        let resolved = web_run_resolve_url(map, "find")?;
        let fetched = fetch_url(&resolved.url, max_bytes, timeout_ms, network_approved)?;
        let text = if looks_like_html(&fetched.content_type, &fetched.body) {
            strip_html_to_text(&fetched.body)
        } else {
            fetched.body
        };
        (resolved.url, text, "fetch")
    };
    let matches = find_text_snippets(&text, pattern, max_matches);

    let mut summary = String::new();
    summary.push_str(&format!("meta.url={}\n", sanitize_meta(&url)));
    summary.push_str(&format!("meta.source={source}\n"));
    summary.push_str(&format!("meta.pattern={}\n", sanitize_meta(pattern)));
    summary.push_str(&format!("meta.count={}\n", matches.len()));
    if matches.is_empty() {
        summary.push_str("No matches found.\n");
    } else {
        summary.push_str("matches:\n");
        for (index, snippet) in matches.iter().enumerate() {
            summary.push_str(&format!("[{}] {}\n", index + 1, snippet));
        }
    }
    Ok(ToolOutput { summary })
}

fn web_run_click(
    turn_id: u64,
    click_index: usize,
    map: &BTreeMap<String, JsonValue>,
    response_length: WebRunResponseLength,
    network_approved: bool,
) -> AppResult<ToolOutput> {
    let (raw_ref, link_id, link) = web_run_click_link(map)?;

    let mut fetch_input = tool_input_from_json_object(map);
    fetch_input.args.insert("url".to_string(), link.url.clone());
    fetch_input.args.remove("ref_id");
    fetch_input.args.remove("id");
    fetch_input.args.remove("link_id");
    mark_network_approved(&mut fetch_input, network_approved);
    let view_options = WebRunViewOptions::from_action(map, response_length);
    let view = fetch_page_view_from_input(&link.url, &fetch_input, Some(view_options))?;
    let mut output = ToolOutput {
        summary: view.summary,
    };
    output.summary.insert_str(
        web_run_meta_insert_index(&output.summary),
        &format!(
            "meta.clicked_ref={}\nmeta.clicked_id={}\nmeta.clicked_text={}\n",
            sanitize_meta(&raw_ref),
            link_id,
            sanitize_meta(&link.text)
        ),
    );
    let refs = store_web_run_click_page(turn_id, click_index, &link.url, view.cache)?;
    append_ref_lines(&mut output.summary, &refs);
    Ok(output)
}

fn web_run_click_link(map: &BTreeMap<String, JsonValue>) -> AppResult<(String, usize, WebRunLink)> {
    let raw_ref = web_run_raw_ref(map, "click")?.to_string();
    let link_id = json_usize(map, "id")
        .or_else(|| json_usize(map, "link_id"))
        .ok_or_else(|| app_error("web_run `click` requires `id`"))?;
    if link_id == 0 {
        return Err(app_error("web_run `click` id must be >= 1"));
    }
    let page = lookup_web_run_page(&raw_ref)?.ok_or_else(|| {
        app_error(format!(
            "web_run `click` could not resolve cached page `{raw_ref}`; call `open` first"
        ))
    })?;
    let link = page
        .links
        .iter()
        .find(|link| link.id == link_id)
        .cloned()
        .ok_or_else(|| {
            app_error(format!(
                "web_run `click` link id {link_id} not found for ref_id `{raw_ref}`"
            ))
        })?;
    Ok((raw_ref, link_id, link))
}

fn web_run_screenshot(map: &BTreeMap<String, JsonValue>) -> AppResult<ToolOutput> {
    let raw_ref = json_string(map, "ref_id")
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| app_error("web_run `screenshot` requires `ref_id`"))?;
    let pageno = json_usize(map, "pageno").unwrap_or(0);
    let page = lookup_web_run_page(raw_ref)?.ok_or_else(|| {
        app_error(format!(
            "web_run `screenshot` could not resolve cached page `{raw_ref}`; call `open` on a PDF first"
        ))
    })?;
    if page.pdf_pages.is_empty() {
        return Err(app_error(
            "web_run `screenshot` is only supported for cached PDF pages",
        ));
    }
    if pageno >= page.pdf_pages.len() {
        return Err(app_error(format!(
            "web_run `screenshot` pageno {pageno} out of range (0..{})",
            page.pdf_pages.len().saturating_sub(1)
        )));
    }

    let content = page.pdf_pages[pageno].join("\n");
    let mut summary = String::new();
    summary.push_str(&format!("meta.ref_id={}\n", sanitize_meta(raw_ref)));
    summary.push_str(&format!("meta.pageno={pageno}\n"));
    summary.push_str(&format!("meta.total_pages={}\n", page.pdf_pages.len()));
    summary.push_str("content:\n");
    if content.trim().is_empty() {
        summary.push_str("(no text extracted)\n");
    } else {
        summary.push_str(content.trim());
        summary.push('\n');
    }
    Ok(ToolOutput { summary })
}

fn web_run_image_query(
    map: &BTreeMap<String, JsonValue>,
    network_approved: bool,
) -> AppResult<ToolOutput> {
    let request = image_query_request(map)?;
    let (results, source, warning) = run_image_query(&request, network_approved)?;

    let mut summary = String::new();
    summary.push_str(&format!("meta.query={}\n", sanitize_meta(&request.query)));
    summary.push_str(&format!("meta.source={source}\n"));
    summary.push_str(&format!("meta.count={}\n", results.len()));
    let mut warnings = Vec::new();
    if let Some(recency) = request.recency {
        warnings.push(format!(
            "Recency filter not enforced (requested last {recency} days)"
        ));
    }
    if let Some(warning) = warning {
        warnings.push(warning);
    }
    if !warnings.is_empty() {
        summary.push_str(&format!(
            "meta.warning={}\n",
            sanitize_meta(&warnings.join("; "))
        ));
    }
    if results.is_empty() {
        summary.push_str("No image results found.\n");
        return Ok(ToolOutput { summary });
    }

    summary.push_str(&format!("Found {} image result(s):\n", results.len()));
    for (index, result) in results.iter().enumerate() {
        let label = result
            .title
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or("image result");
        summary.push_str(&format!("[{}] {label}\n", index + 1));
        summary.push_str(&format!("image: {}\n", result.image));
        if let Some(thumbnail) = result.thumbnail.as_deref() {
            summary.push_str(&format!("thumbnail: {thumbnail}\n"));
        }
        if let Some(url) = result.url.as_deref() {
            summary.push_str(&format!("url: {url}\n"));
        }
        if let Some(source) = result.source.as_deref() {
            summary.push_str(&format!("source: {source}\n"));
        }
        if result.width.is_some() || result.height.is_some() {
            summary.push_str("size: ");
            summary.push_str(result.width.as_deref().unwrap_or("?"));
            summary.push('x');
            summary.push_str(result.height.as_deref().unwrap_or("?"));
            summary.push('\n');
        }
    }
    Ok(ToolOutput { summary })
}

fn fetch_page_view_from_input(
    url: &str,
    input: &ToolInput,
    view_options: Option<WebRunViewOptions>,
) -> AppResult<FetchedPageView> {
    let format = input.get("format").unwrap_or("markdown");
    let max_bytes =
        parse_usize_arg(input, "max_bytes", DEFAULT_FETCH_MAX_BYTES).clamp(1, HARD_FETCH_MAX_BYTES);
    let timeout_ms =
        parse_u64_arg(input, "timeout_ms", DEFAULT_TIMEOUT_MS).clamp(1, HARD_TIMEOUT_MS);
    let fetched = fetch_url(url, max_bytes, timeout_ms, network_approved(input))?;
    render_fetched_page(url, format, fetched, view_options)
}

fn render_fetched_page(
    requested_url: &str,
    format: &str,
    fetched: FetchedUrl,
    view_options: Option<WebRunViewOptions>,
) -> AppResult<FetchedPageView> {
    let is_pdf = looks_like_pdf(&fetched.content_type, &fetched.final_url);
    let links = if !is_pdf && looks_like_html(&fetched.content_type, &fetched.body) {
        extract_links_from_html(&fetched.final_url, &fetched.body, 50)
    } else {
        Vec::new()
    };

    let mut pdf_pages = Vec::new();
    let body = match format.trim().to_ascii_lowercase().as_str() {
        "raw" if !is_pdf => fetched.body.clone(),
        "raw" | "text" | "markdown" | "" if is_pdf => {
            pdf_pages = extract_pdf_text_pages(&fetched.body_bytes)?;
            pdf_pages
                .first()
                .map(|page| page.join("\n"))
                .unwrap_or_default()
        }
        "text" | "markdown" | "" => {
            if looks_like_html(&fetched.content_type, &fetched.body) {
                strip_html_to_text(&fetched.body)
            } else {
                fetched.body.clone()
            }
        }
        other => return Err(app_error(format!("unsupported fetch_url format `{other}`"))),
    };

    let mut summary = String::new();
    summary.push_str(&format!("meta.url={}\n", sanitize_meta(requested_url)));
    summary.push_str(&format!(
        "meta.final_url={}\n",
        sanitize_meta(&fetched.final_url)
    ));
    summary.push_str(&format!("meta.status={}\n", fetched.status));
    summary.push_str(&format!(
        "meta.content_type={}\n",
        sanitize_meta(&fetched.content_type)
    ));
    if is_pdf {
        summary.push_str(&format!("meta.pdf_pages={}\n", pdf_pages.len()));
        summary.push_str("meta.pdf_screenshot=available\n");
    }
    let rendered_window = view_options.map(|options| {
        render_text_window(&body, options.lineno, options.response_length.view_lines())
    });
    if let Some(window) = rendered_window.as_ref() {
        summary.push_str(&format!("meta.line_start={}\n", window.line_start));
        summary.push_str(&format!("meta.line_end={}\n", window.line_end));
        summary.push_str(&format!("meta.total_lines={}\n", window.total_lines));
    }
    summary.push_str(&format!("meta.total_bytes={}\n", fetched.total_bytes));
    summary.push_str(&format!("meta.truncated={}\n", fetched.truncated));
    summary.push_str("content:\n");
    if let Some(window) = rendered_window.as_ref() {
        summary.push_str(window.content.trim());
    } else {
        summary.push_str(body.trim());
    }
    if !summary.ends_with('\n') {
        summary.push('\n');
    }
    append_link_lines(&mut summary, &links);

    Ok(FetchedPageView {
        summary,
        cache: WebRunCachedPage {
            text: body,
            links,
            pdf_pages,
        },
    })
}

fn web_run_state() -> &'static Mutex<WebRunState> {
    WEB_RUN_STATE.get_or_init(|| Mutex::new(WebRunState::default()))
}

fn next_web_run_turn_id() -> AppResult<u64> {
    let mut state = web_run_state()
        .lock()
        .map_err(|_| app_error("web_run state lock is poisoned"))?;
    let turn_id = state.next_turn;
    state.next_turn = state.next_turn.saturating_add(1);
    Ok(turn_id)
}

fn lookup_web_run_url(ref_id: &str) -> AppResult<Option<String>> {
    let state = web_run_state()
        .lock()
        .map_err(|_| app_error("web_run state lock is poisoned"))?;
    Ok(state.urls.get(ref_id).cloned())
}

fn lookup_web_run_page(ref_id: &str) -> AppResult<Option<WebRunCachedPage>> {
    let state = web_run_state()
        .lock()
        .map_err(|_| app_error("web_run state lock is poisoned"))?;
    Ok(state.pages.get(ref_id).cloned())
}

fn store_web_run_search_refs(
    turn_id: u64,
    search_summary: &str,
    start_index: usize,
) -> AppResult<Vec<String>> {
    let refs = extract_search_refs(search_summary);
    if refs.is_empty() {
        return Ok(Vec::new());
    }
    let mut state = web_run_state()
        .lock()
        .map_err(|_| app_error("web_run state lock is poisoned"))?;
    let mut lines = Vec::new();
    for (offset, (compact_ref, url)) in refs.into_iter().enumerate() {
        let scoped_ref = format!("turn{turn_id}search{}", start_index + offset);
        state.urls.insert(compact_ref.clone(), url.clone());
        state.urls.insert(scoped_ref.clone(), url.clone());
        lines.push(format!("web_run_ref: {compact_ref} -> {url}"));
        lines.push(format!("web_run_ref: {scoped_ref} -> {url}"));
    }
    Ok(lines)
}

fn store_web_run_open_page(
    turn_id: u64,
    open_index: usize,
    source_ref: Option<&str>,
    url: &str,
    cached: WebRunCachedPage,
) -> AppResult<Vec<String>> {
    let compact_ref = format!("open{open_index}");
    let scoped_ref = format!("turn{turn_id}open{open_index}");
    let mut state = web_run_state()
        .lock()
        .map_err(|_| app_error("web_run state lock is poisoned"))?;
    for ref_id in [&compact_ref, &scoped_ref, url] {
        state.urls.insert(ref_id.to_string(), url.to_string());
        state.pages.insert(ref_id.to_string(), cached.clone());
    }
    if let Some(source_ref) = source_ref {
        state.urls.insert(source_ref.to_string(), url.to_string());
        state.pages.insert(source_ref.to_string(), cached.clone());
    }
    Ok(vec![
        format!("web_run_ref: {compact_ref} -> cached page for {url}"),
        format!("web_run_ref: {scoped_ref} -> cached page for {url}"),
    ])
}

fn store_web_run_click_page(
    turn_id: u64,
    click_index: usize,
    url: &str,
    cached: WebRunCachedPage,
) -> AppResult<Vec<String>> {
    let compact_ref = format!("click{click_index}");
    let scoped_ref = format!("turn{turn_id}click{click_index}");
    let mut state = web_run_state()
        .lock()
        .map_err(|_| app_error("web_run state lock is poisoned"))?;
    for ref_id in [&compact_ref, &scoped_ref, url] {
        state.urls.insert(ref_id.to_string(), url.to_string());
        state.pages.insert(ref_id.to_string(), cached.clone());
    }
    Ok(vec![
        format!("web_run_ref: {compact_ref} -> cached page for {url}"),
        format!("web_run_ref: {scoped_ref} -> cached page for {url}"),
    ])
}

fn extract_search_refs(search_summary: &str) -> Vec<(String, String)> {
    let mut refs = Vec::new();
    let mut current_url: Option<String> = None;
    for line in search_summary.lines() {
        if let Some(url) = line.strip_prefix("url: ").map(str::trim) {
            current_url = Some(url.to_string());
        } else if let Some(ref_id) = line.strip_prefix("ref_id: ").map(str::trim) {
            if let Some(url) = current_url.take() {
                refs.push((ref_id.to_string(), url));
            }
        }
    }
    refs
}

fn append_ref_lines(summary: &mut String, refs: &[String]) {
    if refs.is_empty() {
        return;
    }
    if !summary.ends_with('\n') {
        summary.push('\n');
    }
    summary.push_str("web_run refs:\n");
    for line in refs {
        summary.push_str(line);
        summary.push('\n');
    }
}

fn web_run_meta_insert_index(summary: &str) -> usize {
    let mut index = 0usize;
    for line in summary.split_inclusive('\n') {
        if line.starts_with("meta.") {
            index += line.len();
        } else {
            break;
        }
    }
    index
}

fn find_text_snippets(text: &str, pattern: &str, max_matches: usize) -> Vec<String> {
    let haystack = text.to_ascii_lowercase();
    let needle = pattern.to_ascii_lowercase();
    let mut matches = Vec::new();
    let mut offset = 0usize;
    while matches.len() < max_matches {
        let Some(relative) = haystack[offset..].find(&needle) else {
            break;
        };
        let absolute = offset + relative;
        let match_end = absolute.saturating_add(needle.len()).min(text.len());
        let context_start = text[..absolute]
            .rfind('\n')
            .map(|index| index + 1)
            .unwrap_or_else(|| absolute.saturating_sub(160));
        let context_end = text[match_end..]
            .find('\n')
            .map(|index| match_end + index)
            .unwrap_or_else(|| (match_end + 160).min(text.len()));
        if let Some(snippet) = text.get(context_start..context_end) {
            let compact = snippet.split_whitespace().collect::<Vec<_>>().join(" ");
            if !compact.is_empty() {
                matches.push(compact);
            }
        }
        offset = match_end.max(absolute + 1);
    }
    matches
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RenderedTextWindow {
    line_start: usize,
    line_end: usize,
    total_lines: usize,
    content: String,
}

fn render_text_window(text: &str, lineno: usize, view_lines: usize) -> RenderedTextWindow {
    let lines = text.lines().collect::<Vec<_>>();
    let total_lines = lines.len();
    if total_lines == 0 {
        return RenderedTextWindow {
            line_start: 1,
            line_end: 0,
            total_lines,
            content: "(no content)".to_string(),
        };
    }

    let lineno = lineno.max(1);
    let view_lines = view_lines.max(1);
    let line_start = if lineno > total_lines {
        total_lines
            .saturating_sub(view_lines.saturating_sub(1))
            .max(1)
    } else {
        lineno
    };
    let line_end = (line_start + view_lines - 1).min(total_lines);
    let mut content = String::new();
    for line_no in line_start..=line_end {
        content.push_str(&format!("{line_no:>4} {}\n", lines[line_no - 1]));
    }
    if content.ends_with('\n') {
        content.pop();
    }

    RenderedTextWindow {
        line_start,
        line_end,
        total_lines,
        content,
    }
}

fn append_web_run_result(
    summary: &mut String,
    action: &str,
    index: usize,
    output: ToolOutput,
) -> AppResult<()> {
    summary.push_str(&format!("\n--- web_run {action}[{index}] ---\n"));
    summary.push_str(output.summary.trim_end());
    summary.push('\n');
    Ok(())
}

fn is_direct_http_url(value: &str) -> bool {
    value.starts_with("http://") || value.starts_with("https://")
}

fn required_trimmed<'a>(input: &'a ToolInput, key: &str, message: &str) -> AppResult<&'a str> {
    input
        .get(key)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| app_error(message))
}

pub(crate) fn network_permission_target_for_tool(
    tool_name: &str,
    input: &ToolInput,
    network: &NetworkConfig,
) -> Option<String> {
    let mut hosts = BTreeSet::new();
    collect_network_prompt_hosts(tool_name, input, network, &mut hosts);
    if hosts.is_empty() {
        None
    } else {
        Some(format!(
            "web fetch: {}",
            hosts.into_iter().collect::<Vec<_>>().join(", ")
        ))
    }
}

fn collect_network_prompt_hosts(
    tool_name: &str,
    input: &ToolInput,
    network: &NetworkConfig,
    hosts: &mut BTreeSet<String>,
) {
    match tool_name {
        "fetch_url" => {
            if let Some(url) = input.get("url") {
                collect_prompt_host_from_url(url, network, hosts);
            }
        }
        "web_search" => {
            if let Ok(request) = search_request(input) {
                collect_prompt_host_from_url(&search_url(&request.query), network, hosts);
            }
        }
        "finance" => {
            if let Ok(request) = finance_request(input) {
                collect_prompt_host_from_url(&finance_url(&request.ticker), network, hosts);
            }
        }
        "web_run" => collect_web_run_prompt_hosts(input, network, hosts),
        _ => {}
    }
}

fn collect_web_run_prompt_hosts(
    input: &ToolInput,
    network: &NetworkConfig,
    hosts: &mut BTreeSet<String>,
) {
    if let Some(raw) = input.get("search_query") {
        if let Ok(actions) = web_run_action_objects("search_query", raw) {
            for action in actions {
                let search_input = tool_input_from_json_object(&action);
                collect_network_prompt_hosts("web_search", &search_input, network, hosts);
            }
        }
    }
    if let Some(raw) = input.get("finance") {
        if let Ok(actions) = web_run_action_objects("finance", raw) {
            for action in actions {
                let finance_input = tool_input_from_json_object(&action);
                collect_network_prompt_hosts("finance", &finance_input, network, hosts);
            }
        }
    }
    if let Some(raw) = input.get("image_query") {
        if let Ok(actions) = web_run_action_objects("image_query", raw) {
            for action in actions {
                collect_image_query_prompt_hosts(&action, network, hosts);
            }
        }
    }
    if let Some(raw) = input.get("click") {
        if let Ok(actions) = web_run_action_objects("click", raw) {
            for action in actions {
                if let Ok((_, _, link)) = web_run_click_link(&action) {
                    collect_prompt_host_from_url(&link.url, network, hosts);
                }
            }
        }
    }
    for action_name in ["open", "find"] {
        if let Some(raw) = input.get(action_name) {
            if let Ok(actions) = web_run_action_objects(action_name, raw) {
                for action in actions {
                    if let Ok(resolved) = web_run_resolve_url(&action, action_name) {
                        collect_prompt_host_from_url(&resolved.url, network, hosts);
                    }
                }
            }
        }
    }
}

fn collect_image_query_prompt_hosts(
    map: &BTreeMap<String, JsonValue>,
    network: &NetworkConfig,
    hosts: &mut BTreeSet<String>,
) {
    let Ok(request) = image_query_request(map) else {
        return;
    };
    if let Some(template) = image_search_template() {
        collect_prompt_host_from_url(
            &template.replace("{query}", &url_encode(&request.query)),
            network,
            hosts,
        );
    } else {
        collect_prompt_host_from_url("https://duckduckgo.com/", network, hosts);
    }
}

fn collect_prompt_host_from_url(url: &str, network: &NetworkConfig, hosts: &mut BTreeSet<String>) {
    let Ok(parsed) = parse_http_url(url) else {
        return;
    };
    if is_private_or_local_host(&parsed.host) && !env_flag("DSCODE_ALLOW_LOCAL_FETCH") {
        return;
    }
    if decide(network, &parsed.host) == NetworkDecision::Prompt {
        hosts.insert(parsed.host);
    }
}

fn fetch_url(
    url: &str,
    max_bytes: usize,
    timeout_ms: u64,
    network_approved: bool,
) -> AppResult<FetchedUrl> {
    let parsed = parse_http_url(url)?;
    validate_network_target(&parsed, network_approved)?;
    if parsed.scheme == "http" {
        return fetch_http_url(url, &parsed, max_bytes, timeout_ms);
    }
    fetch_https_with_curl(url, max_bytes, timeout_ms)
}

pub(crate) fn fetch_url_text(
    url: &str,
    max_bytes: usize,
    timeout_ms: u64,
    network_approved: bool,
) -> AppResult<String> {
    Ok(fetch_url(url, max_bytes, timeout_ms, network_approved)?.body)
}

pub(crate) fn fetch_url_bytes(
    url: &str,
    max_bytes: usize,
    timeout_ms: u64,
    network_approved: bool,
) -> AppResult<FetchedUrlBytes> {
    let fetched = fetch_url(url, max_bytes, timeout_ms, network_approved)?;
    Ok(FetchedUrlBytes {
        final_url: fetched.final_url,
        status: fetched.status,
        body_bytes: fetched.body_bytes,
        total_bytes: fetched.total_bytes,
        truncated: fetched.truncated,
    })
}

fn parse_http_url(url: &str) -> AppResult<ParsedUrl> {
    let trimmed = url.trim();
    let (scheme, rest) = trimmed
        .split_once("://")
        .ok_or_else(|| app_error("URL must include http:// or https:// scheme"))?;
    let scheme = scheme.to_ascii_lowercase();
    if !matches!(scheme.as_str(), "http" | "https") {
        return Err(app_error("only http:// and https:// URLs are supported"));
    }
    if rest.contains('@') {
        return Err(app_error("URLs with userinfo are not supported"));
    }
    let (authority, path_query) = match rest.find(['/', '?', '#']) {
        Some(index) => (&rest[..index], &rest[index..]),
        None => (rest, "/"),
    };
    if authority.trim().is_empty() {
        return Err(app_error("URL host cannot be empty"));
    }
    let (host, port) = parse_authority(authority, &scheme)?;
    let path_query = if path_query.starts_with('?') {
        format!("/{path_query}")
    } else if path_query.is_empty() {
        "/".to_string()
    } else {
        path_query.to_string()
    };
    Ok(ParsedUrl {
        scheme,
        host,
        port,
        path_query,
    })
}

fn parse_authority(authority: &str, scheme: &str) -> AppResult<(String, u16)> {
    if authority.starts_with('[') {
        let end = authority
            .find(']')
            .ok_or_else(|| app_error("invalid bracketed IPv6 URL host"))?;
        let host = authority[1..end].to_string();
        let port = if let Some(port) = authority[end + 1..].strip_prefix(':') {
            parse_port(port)?
        } else {
            default_port(scheme)
        };
        return Ok((host, port));
    }

    let (host, port) = if let Some((host, port)) = authority.rsplit_once(':') {
        if port.chars().all(|ch| ch.is_ascii_digit()) {
            (host.to_string(), parse_port(port)?)
        } else {
            (authority.to_string(), default_port(scheme))
        }
    } else {
        (authority.to_string(), default_port(scheme))
    };
    Ok((host.to_ascii_lowercase(), port))
}

fn default_port(scheme: &str) -> u16 {
    if scheme == "https" {
        443
    } else {
        80
    }
}

fn parse_port(port: &str) -> AppResult<u16> {
    port.parse::<u16>()
        .map_err(|_| app_error(format!("invalid URL port `{port}`")))
}

fn validate_network_target(url: &ParsedUrl, network_approved: bool) -> AppResult<()> {
    let config = crate::config::load::load_or_default()?;
    if is_private_or_local_host(&url.host) && !env_flag("DSCODE_ALLOW_LOCAL_FETCH") {
        audit_decision(
            &config.network,
            &url.host,
            "web_fetch",
            NetworkDecision::Deny,
        );
        return Err(app_error(format!(
            "fetch_url/web_search blocked local or private host `{}`; set DSCODE_ALLOW_LOCAL_FETCH=1 for trusted local testing",
            url.host
        )));
    }
    let decision = decide(&config.network, &url.host);
    audit_decision(&config.network, &url.host, "web_fetch", decision);
    match decision {
        NetworkDecision::Allow => Ok(()),
        NetworkDecision::Deny => Err(app_error(format!(
            "network policy denied host `{}` for fetch_url/web_search",
            url.host
        ))),
        NetworkDecision::Prompt if network_approved => Ok(()),
        NetworkDecision::Prompt => Err(app_error(format!(
            "network policy requires approval for host `{}`; configure network.allow or set network.default = \"allow\" for non-interactive runs",
            url.host
        ))),
    }
}

fn is_private_or_local_host(host: &str) -> bool {
    let host = host
        .trim_matches(|ch| ch == '[' || ch == ']')
        .to_ascii_lowercase();
    if matches!(host.as_str(), "localhost" | "::1" | "0.0.0.0") {
        return true;
    }
    if host.starts_with("127.") || host.starts_with("10.") || host.starts_with("192.168.") {
        return true;
    }
    if let Some(rest) = host.strip_prefix("172.") {
        if let Some(octet) = rest
            .split('.')
            .next()
            .and_then(|value| value.parse::<u8>().ok())
        {
            return (16..=31).contains(&octet);
        }
    }
    false
}

fn fetch_http_url(
    original_url: &str,
    parsed: &ParsedUrl,
    max_bytes: usize,
    timeout_ms: u64,
) -> AppResult<FetchedUrl> {
    let mut stream = TcpStream::connect((parsed.host.as_str(), parsed.port))?;
    let timeout = Duration::from_millis(timeout_ms);
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    let host_header = if parsed.port == default_port(&parsed.scheme) {
        parsed.host.clone()
    } else {
        format!("{}:{}", parsed.host, parsed.port)
    };
    let request = format!(
        "GET {} HTTP/1.1\r\nHost: {}\r\nUser-Agent: {}\r\nAccept: text/html,text/plain,application/json,*/*;q=0.5\r\nConnection: close\r\n\r\n",
        parsed.path_query, host_header, USER_AGENT
    );
    stream.write_all(request.as_bytes())?;
    let mut response = Vec::new();
    stream.read_to_end(&mut response)?;
    parse_http_response(original_url, response, max_bytes)
}

fn parse_http_response(
    original_url: &str,
    response: Vec<u8>,
    max_bytes: usize,
) -> AppResult<FetchedUrl> {
    let header_end = find_bytes(&response, b"\r\n\r\n")
        .map(|index| index + 4)
        .or_else(|| find_bytes(&response, b"\n\n").map(|index| index + 2))
        .ok_or_else(|| app_error("HTTP response missing header terminator"))?;
    let headers = String::from_utf8_lossy(&response[..header_end]);
    let mut lines = headers.lines();
    let status_line = lines
        .next()
        .ok_or_else(|| app_error("HTTP response missing status line"))?;
    let status = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|value| value.parse::<u16>().ok())
        .ok_or_else(|| app_error("HTTP response status code was not parseable"))?;
    let mut content_type = "application/octet-stream".to_string();
    let mut transfer_encoding = String::new();
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            if name.eq_ignore_ascii_case("content-type") {
                content_type = value.trim().to_string();
            } else if name.eq_ignore_ascii_case("transfer-encoding") {
                transfer_encoding = value.trim().to_ascii_lowercase();
            }
        }
    }
    let body_bytes = if transfer_encoding.contains("chunked") {
        decode_chunked_body(&response[header_end..])?
    } else {
        response[header_end..].to_vec()
    };
    Ok(fetched_from_bytes(
        original_url,
        status,
        content_type,
        body_bytes,
        max_bytes,
    ))
}

fn fetch_https_with_curl(url: &str, max_bytes: usize, timeout_ms: u64) -> AppResult<FetchedUrl> {
    let timeout_seconds = (timeout_ms as f64 / 1000.0).max(0.001).to_string();
    let output = Command::new("curl")
        .args([
            "-L",
            "-sS",
            "--max-time",
            &timeout_seconds,
            "-A",
            USER_AGENT,
            "-H",
            "Accept: text/html,text/plain,application/json,*/*;q=0.5",
            "-w",
            &format!("{CURL_META_MARKER}%{{http_code}}\t%{{url_effective}}\t%{{content_type}}"),
            "--",
            url,
        ])
        .output()?;
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if !output.status.success() {
        return Err(app_error(format!(
            "curl failed for `{}`: {}",
            url,
            if stderr.is_empty() {
                output.status.to_string()
            } else {
                stderr
            }
        )));
    }

    let Some(marker_index) = rfind_bytes(&output.stdout, CURL_META_MARKER.as_bytes()) else {
        return Err(app_error("curl response missing metadata marker"));
    };
    let body = output.stdout[..marker_index].to_vec();
    let meta = String::from_utf8_lossy(&output.stdout[marker_index + CURL_META_MARKER.len()..])
        .to_string();
    let mut parts = meta.splitn(3, '\t');
    let status = parts
        .next()
        .and_then(|value| value.parse::<u16>().ok())
        .unwrap_or(0);
    let final_url = parts.next().unwrap_or(url).trim();
    let content_type = parts
        .next()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("application/octet-stream");
    let mut fetched = fetched_from_bytes(url, status, content_type.to_string(), body, max_bytes);
    fetched.final_url = final_url.to_string();
    Ok(fetched)
}

fn fetched_from_bytes(
    final_url: &str,
    status: u16,
    content_type: String,
    bytes: Vec<u8>,
    max_bytes: usize,
) -> FetchedUrl {
    let total_bytes = bytes.len();
    let truncated = total_bytes > max_bytes;
    let visible = if truncated {
        &bytes[..max_bytes]
    } else {
        bytes.as_slice()
    };
    let body = String::from_utf8_lossy(visible).to_string();
    FetchedUrl {
        final_url: final_url.to_string(),
        status,
        content_type,
        body,
        body_bytes: bytes,
        total_bytes,
        truncated,
    }
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn rfind_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .rposition(|window| window == needle)
}

fn decode_chunked_body(body: &[u8]) -> AppResult<Vec<u8>> {
    let mut index = 0usize;
    let mut decoded = Vec::new();
    loop {
        let line_end = find_bytes(&body[index..], b"\r\n")
            .ok_or_else(|| app_error("chunked response missing chunk size line"))?
            + index;
        let size_text = String::from_utf8_lossy(&body[index..line_end]);
        let size_hex = size_text.split(';').next().unwrap_or("").trim();
        let size = usize::from_str_radix(size_hex, 16)
            .map_err(|_| app_error("chunked response has invalid chunk size"))?;
        index = line_end + 2;
        if size == 0 {
            break;
        }
        if index + size > body.len() {
            return Err(app_error("chunked response ended before chunk body"));
        }
        decoded.extend_from_slice(&body[index..index + size]);
        index += size;
        if body.get(index..index + 2) == Some(b"\r\n") {
            index += 2;
        }
    }
    Ok(decoded)
}

fn search_request(input: &ToolInput) -> AppResult<SearchRequest> {
    let mut query = input
        .get("query")
        .or_else(|| input.get("q"))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    let mut max_results = parse_usize_arg(input, "max_results", DEFAULT_SEARCH_MAX_RESULTS)
        .clamp(1, HARD_SEARCH_MAX_RESULTS);
    let mut timeout_ms =
        parse_u64_arg(input, "timeout_ms", DEFAULT_TIMEOUT_MS).clamp(1, HARD_TIMEOUT_MS);

    if let Some(search_query) = input.get("search_query") {
        if let Some(nested) = first_search_query(search_query)? {
            if query.is_none() {
                query = nested.query;
            }
            if let Some(value) = nested.max_results {
                max_results = value.clamp(1, HARD_SEARCH_MAX_RESULTS);
            }
            if let Some(value) = nested.timeout_ms {
                timeout_ms = value.clamp(1, HARD_TIMEOUT_MS);
            }
        }
    }

    Ok(SearchRequest {
        query: query
            .ok_or_else(|| app_error("web_search requires `query`, `q`, or `search_query[0].q`"))?,
        max_results,
        timeout_ms,
    })
}

struct NestedSearchQuery {
    query: Option<String>,
    max_results: Option<usize>,
    timeout_ms: Option<u64>,
}

fn first_search_query(input: &str) -> AppResult<Option<NestedSearchQuery>> {
    let value = parse_json_value(input.trim())?;
    let first = match value {
        JsonValue::Array(values) => values.into_iter().next(),
        JsonValue::Object(_) => Some(value),
        _ => None,
    };
    let Some(JsonValue::Object(map)) = first else {
        return Ok(None);
    };
    Ok(Some(NestedSearchQuery {
        query: json_string(&map, "q")
            .or_else(|| json_string(&map, "query"))
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned),
        max_results: json_usize(&map, "max_results"),
        timeout_ms: json_u64(&map, "timeout_ms"),
    }))
}

fn finance_request(input: &ToolInput) -> AppResult<FinanceRequest> {
    let raw = input
        .get("ticker")
        .or_else(|| input.get("symbol"))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| app_error("finance requires `ticker` or `symbol`"))?;
    let kind = input.get("type").unwrap_or_default();
    let market = input.get("market").unwrap_or_default();
    let ticker = normalize_finance_ticker(raw, kind, market)?;
    let timeout_ms = parse_u64_arg(input, "timeout_ms", 10_000).clamp(1_000, HARD_TIMEOUT_MS);
    Ok(FinanceRequest { ticker, timeout_ms })
}

fn image_query_request(map: &BTreeMap<String, JsonValue>) -> AppResult<ImageQueryRequest> {
    let query = json_string(map, "q")
        .or_else(|| json_string(map, "query"))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| app_error("web_run `image_query` requires `q`"))?
        .to_string();
    let max_results = json_usize(map, "max_results")
        .unwrap_or(DEFAULT_SEARCH_MAX_RESULTS)
        .clamp(1, HARD_SEARCH_MAX_RESULTS);
    let timeout_ms = json_u64(map, "timeout_ms")
        .unwrap_or(DEFAULT_TIMEOUT_MS)
        .clamp(1, HARD_TIMEOUT_MS);
    let domains = json_string_array(map, "domains");
    let recency = json_u64(map, "recency").filter(|value| *value > 0);
    Ok(ImageQueryRequest {
        query,
        max_results,
        timeout_ms,
        domains,
        recency,
    })
}

fn normalize_finance_ticker(raw: &str, kind: &str, market: &str) -> AppResult<String> {
    let ticker = raw.trim().to_ascii_uppercase();
    if ticker.is_empty() || ticker.len() > 32 {
        return Err(app_error("finance ticker must be 1-32 characters"));
    }
    if !ticker
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '^' | '='))
    {
        return Err(app_error(
            "finance ticker contains unsupported characters; use letters, numbers, '.', '-', '^', or '='",
        ));
    }
    let hint = format!("{} {}", kind, market).to_ascii_lowercase();
    if hint.contains("crypto")
        && !ticker.contains('-')
        && !ticker.contains('=')
        && !ticker.contains('.')
    {
        return Ok(format!("{ticker}-USD"));
    }
    Ok(ticker)
}

fn finance_url(ticker: &str) -> String {
    if let Some(template) = finance_template() {
        return template
            .replace("{ticker}", &url_encode(ticker))
            .replace("{symbol}", &url_encode(ticker));
    }
    format!(
        "https://query1.finance.yahoo.com/v7/finance/quote?symbols={}",
        url_encode(ticker)
    )
}

fn finance_template() -> Option<String> {
    std::env::var("DSCODE_FINANCE_URL_TEMPLATE")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn run_image_query(
    request: &ImageQueryRequest,
    network_approved: bool,
) -> AppResult<(Vec<ImageQueryResult>, &'static str, Option<String>)> {
    let (body, source) = if let Some(template) = image_search_template() {
        let url = template.replace("{query}", &url_encode(&request.query));
        (
            fetch_url(
                &url,
                SEARCH_FETCH_MAX_BYTES,
                request.timeout_ms,
                network_approved,
            )?
            .body,
            "custom",
        )
    } else {
        let encoded = url_encode(&request.query);
        let seed_url = format!("https://duckduckgo.com/?q={encoded}&iax=images&ia=images");
        let seed = fetch_url(
            &seed_url,
            SEARCH_FETCH_MAX_BYTES,
            request.timeout_ms,
            network_approved,
        )?;
        let vqd = extract_duckduckgo_vqd(&seed.body)
            .ok_or_else(|| app_error("failed to extract DuckDuckGo image token `vqd`"))?;
        let api_url =
            format!("https://duckduckgo.com/i.js?l=us-en&o=json&q={encoded}&vqd={vqd}&p=1");
        (
            fetch_url(
                &api_url,
                SEARCH_FETCH_MAX_BYTES,
                request.timeout_ms,
                network_approved,
            )?
            .body,
            "duckduckgo_images",
        )
    };

    let mut results = parse_image_query_results(&body, request.max_results)?;
    let warning = if request.domains.is_empty() {
        None
    } else {
        let before = results.len();
        results.retain(|result| match result.url.as_deref() {
            Some(url) => domain_matches_url(url, &request.domains),
            None => true,
        });
        if before != results.len() {
            Some("Filtered image results by domain list".to_string())
        } else {
            None
        }
    };
    results.truncate(request.max_results);
    Ok((results, source, warning))
}

fn image_search_template() -> Option<String> {
    std::env::var("DSCODE_IMAGE_SEARCH_URL_TEMPLATE")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn extract_duckduckgo_vqd(html: &str) -> Option<String> {
    for (prefix, suffix) in [("vqd='", "'"), ("vqd=\"", "\"")] {
        if let Some(start) = html.find(prefix) {
            let rest = &html[start + prefix.len()..];
            let end = rest.find(suffix)?;
            let token = rest[..end].trim();
            if !token.is_empty() {
                return Some(token.to_string());
            }
        }
    }
    if let Some(start) = html.find("vqd=") {
        let rest = &html[start + 4..];
        let token: String = rest
            .chars()
            .take_while(|ch| ch.is_ascii_alphanumeric() || *ch == '-' || *ch == '_')
            .collect();
        if !token.is_empty() {
            return Some(token);
        }
    }
    None
}

fn parse_image_query_results(body: &str, max_results: usize) -> AppResult<Vec<ImageQueryResult>> {
    let parsed = parse_json_value(body)?;
    let entries = match &parsed {
        JsonValue::Object(root) => root
            .get("results")
            .or_else(|| root.get("images"))
            .and_then(|value| match value {
                JsonValue::Array(items) => Some(items.as_slice()),
                _ => None,
            })
            .ok_or_else(|| app_error("image_query response missing `results` array"))?,
        JsonValue::Array(items) => items.as_slice(),
        _ => {
            return Err(app_error(
                "image_query response must be a JSON object or array",
            ))
        }
    };

    let mut results = Vec::new();
    for entry in entries {
        if results.len() >= max_results {
            break;
        }
        let Some(map) = json_object_ref(entry) else {
            continue;
        };
        let image = json_string(map, "image")
            .or_else(|| json_string(map, "image_url"))
            .or_else(|| json_string(map, "contentUrl"))
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let Some(image) = image else {
            continue;
        };
        results.push(ImageQueryResult {
            image: image.to_string(),
            thumbnail: json_string(map, "thumbnail")
                .or_else(|| json_string(map, "thumbnailUrl"))
                .map(str::to_string),
            title: json_string(map, "title").map(str::to_string),
            url: json_string(map, "url")
                .or_else(|| json_string(map, "sourceUrl"))
                .or_else(|| json_string(map, "page"))
                .map(str::to_string),
            source: json_string(map, "source").map(str::to_string),
            width: json_number_or_string(map, "width"),
            height: json_number_or_string(map, "height"),
        });
    }
    Ok(results)
}

fn parse_finance_quote(body: &str, ticker: &str) -> AppResult<FinanceQuote> {
    let parsed = parse_json_value(body)?;
    let result = json_path(&parsed, &["quoteResponse", "result"])
        .and_then(|value| match value {
            JsonValue::Array(items) => items.first(),
            _ => None,
        })
        .and_then(json_object_ref)
        .ok_or_else(|| app_error(format!("Unknown finance ticker `{ticker}`")))?;

    let symbol = json_string(result, "symbol")
        .map(str::to_string)
        .unwrap_or_else(|| ticker.to_string());
    let price = json_number_or_string(result, "regularMarketPrice")
        .or_else(|| json_number_or_string(result, "postMarketPrice"))
        .or_else(|| json_number_or_string(result, "preMarketPrice"));

    Ok(FinanceQuote {
        symbol,
        name: json_string(result, "shortName")
            .or_else(|| json_string(result, "longName"))
            .map(str::to_string),
        price,
        change: json_number_or_string(result, "regularMarketChange"),
        change_percent: json_number_or_string(result, "regularMarketChangePercent"),
        currency: json_string(result, "currency").map(str::to_string),
        exchange: json_string(result, "fullExchangeName")
            .or_else(|| json_string(result, "exchange"))
            .map(str::to_string),
        market_state: json_string(result, "marketState").map(str::to_string),
    })
}

fn json_path<'a>(value: &'a JsonValue, path: &[&str]) -> Option<&'a JsonValue> {
    let mut current = value;
    for segment in path {
        current = match current {
            JsonValue::Object(map) => map.get(*segment)?,
            _ => return None,
        };
    }
    Some(current)
}

fn json_object_ref(value: &JsonValue) -> Option<&BTreeMap<String, JsonValue>> {
    match value {
        JsonValue::Object(map) => Some(map),
        _ => None,
    }
}

fn json_number_or_string(map: &BTreeMap<String, JsonValue>, key: &str) -> Option<String> {
    match map.get(key) {
        Some(JsonValue::Number(value)) => Some(value.clone()),
        Some(JsonValue::String(value)) if !value.trim().is_empty() => Some(value.clone()),
        _ => None,
    }
}

fn json_string<'a>(map: &'a BTreeMap<String, JsonValue>, key: &str) -> Option<&'a str> {
    match map.get(key) {
        Some(JsonValue::String(value)) => Some(value.as_str()),
        _ => None,
    }
}

fn json_string_array(map: &BTreeMap<String, JsonValue>, key: &str) -> Vec<String> {
    match map.get(key) {
        Some(JsonValue::Array(items)) => items
            .iter()
            .filter_map(|item| match item {
                JsonValue::String(value) => {
                    let value = value.trim();
                    (!value.is_empty()).then(|| value.to_string())
                }
                _ => None,
            })
            .collect(),
        Some(JsonValue::String(value)) => value
            .split([',', ';'])
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .collect(),
        _ => Vec::new(),
    }
}

fn json_usize(map: &BTreeMap<String, JsonValue>, key: &str) -> Option<usize> {
    match map.get(key) {
        Some(JsonValue::Number(value)) => value.parse().ok(),
        Some(JsonValue::String(value)) => value.parse().ok(),
        _ => None,
    }
}

fn json_u64(map: &BTreeMap<String, JsonValue>, key: &str) -> Option<u64> {
    match map.get(key) {
        Some(JsonValue::Number(value)) => value.parse().ok(),
        Some(JsonValue::String(value)) => value.parse().ok(),
        _ => None,
    }
}

fn domain_matches_url(url: &str, domains: &[String]) -> bool {
    if domains.is_empty() {
        return true;
    }
    let Ok(parsed) = parse_http_url(url) else {
        return false;
    };
    let host = parsed.host.trim_start_matches("www.");
    domains.iter().any(|domain| {
        let domain = domain.trim().trim_start_matches("www.");
        !domain.is_empty() && (host == domain || host.ends_with(&format!(".{domain}")))
    })
}

fn search_url(query: &str) -> String {
    if let Some(template) = search_template() {
        return template.replace("{query}", &url_encode(query));
    }
    format!("https://html.duckduckgo.com/html/?q={}", url_encode(query))
}

fn search_template() -> Option<String> {
    std::env::var("DSCODE_WEB_SEARCH_URL_TEMPLATE")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn parse_search_results(html: &str, max_results: usize) -> Vec<SearchResult> {
    let mut results = Vec::new();
    let mut rest = html;
    while results.len() < max_results {
        let Some(anchor_start) = find_anchor_start(rest) else {
            break;
        };
        rest = &rest[anchor_start..];
        let Some(tag_end) = rest.find('>') else {
            break;
        };
        let tag = &rest[..=tag_end];
        let Some(href) = extract_attr(tag, "href").and_then(clean_result_url) else {
            rest = &rest[tag_end + 1..];
            continue;
        };
        let after_tag = &rest[tag_end + 1..];
        let Some(anchor_end) = after_tag.find("</a>") else {
            break;
        };
        let title = strip_html_to_text(&after_tag[..anchor_end]);
        if title.is_empty() {
            rest = &after_tag[anchor_end + 4..];
            continue;
        }
        let after_anchor = &after_tag[anchor_end + 4..];
        let snippet_end = after_anchor
            .find("<a")
            .unwrap_or(after_anchor.len())
            .min(900);
        let snippet = strip_html_to_text(&after_anchor[..snippet_end]);
        if !results
            .iter()
            .any(|result: &SearchResult| result.url == href)
        {
            results.push(SearchResult {
                title,
                url: href,
                snippet: snippet.chars().take(240).collect(),
            });
        }
        rest = after_anchor;
    }
    results
}

fn extract_links_from_html(base_url: &str, html: &str, max_links: usize) -> Vec<WebRunLink> {
    let mut links = Vec::new();
    let mut rest = html;
    while links.len() < max_links {
        let Some(anchor_start) = find_anchor_start(rest) else {
            break;
        };
        rest = &rest[anchor_start..];
        let Some(tag_end) = rest.find('>') else {
            break;
        };
        let tag = &rest[..=tag_end];
        let Some(href) = extract_attr(tag, "href")
            .as_deref()
            .and_then(|href| normalize_link_url(base_url, href))
        else {
            rest = &rest[tag_end + 1..];
            continue;
        };
        let after_tag = &rest[tag_end + 1..];
        let Some(anchor_end) = after_tag.to_ascii_lowercase().find("</a>") else {
            break;
        };
        let mut text = strip_html_to_text(&after_tag[..anchor_end]);
        if text.is_empty() {
            text = href.clone();
        }
        let text = compact_link_text(&text);
        if !links.iter().any(|link: &WebRunLink| link.url == href) {
            links.push(WebRunLink {
                id: links.len() + 1,
                url: href,
                text,
            });
        }
        rest = &after_tag[anchor_end + 4..];
    }
    links
}

fn find_anchor_start(input: &str) -> Option<usize> {
    let lower = input.to_ascii_lowercase();
    let mut best = lower.find("<a ");
    if let Some(index) = lower.find("<a\n") {
        best = Some(best.map_or(index, |current| current.min(index)));
    }
    best
}

fn normalize_link_url(base_url: &str, href: &str) -> Option<String> {
    let href = decode_html_entities(href).trim().to_string();
    if href.is_empty() || href.starts_with('#') {
        return None;
    }
    let lower = href.to_ascii_lowercase();
    if lower.starts_with("javascript:")
        || lower.starts_with("mailto:")
        || lower.starts_with("tel:")
        || lower.starts_with("data:")
    {
        return None;
    }
    if href.starts_with("http://") || href.starts_with("https://") {
        return Some(href);
    }

    let parsed = parse_http_url(base_url).ok()?;
    let origin = parsed_url_origin(&parsed);
    if href.starts_with("//") {
        return Some(format!("{}:{href}", parsed.scheme));
    }
    if href.starts_with('/') {
        return Some(format!("{origin}{}", normalize_path_with_suffix(&href)));
    }

    let base_path = parsed
        .path_query
        .split(['?', '#'])
        .next()
        .filter(|value| !value.is_empty())
        .unwrap_or("/");
    if href.starts_with('?') {
        return Some(format!("{origin}{base_path}{href}"));
    }
    let base_dir = if base_path.ends_with('/') {
        base_path.to_string()
    } else {
        match base_path.rsplit_once('/') {
            Some(("", _)) | None => "/".to_string(),
            Some((dir, _)) => format!("{dir}/"),
        }
    };
    Some(format!(
        "{origin}{}",
        normalize_path_with_suffix(&format!("{base_dir}{href}"))
    ))
}

fn parsed_url_origin(parsed: &ParsedUrl) -> String {
    if parsed.port == default_port(&parsed.scheme) {
        format!("{}://{}", parsed.scheme, parsed.host)
    } else {
        format!("{}://{}:{}", parsed.scheme, parsed.host, parsed.port)
    }
}

fn normalize_path_with_suffix(path: &str) -> String {
    let split_at = path.find(['?', '#']).unwrap_or(path.len());
    let path_part = &path[..split_at];
    let suffix = &path[split_at..];
    let mut parts = Vec::new();
    for part in path_part.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    let mut normalized = format!("/{}", parts.join("/"));
    if path_part.ends_with('/') && !normalized.ends_with('/') {
        normalized.push('/');
    }
    normalized.push_str(suffix);
    normalized
}

fn compact_link_text(text: &str) -> String {
    let compact = text.split_whitespace().collect::<Vec<_>>().join(" ");
    compact.chars().take(180).collect()
}

fn append_link_lines(summary: &mut String, links: &[WebRunLink]) {
    if links.is_empty() {
        return;
    }
    if !summary.ends_with('\n') {
        summary.push('\n');
    }
    summary.push_str("links:\n");
    for link in links {
        summary.push_str(&format!("[{}] {} -> {}\n", link.id, link.text, link.url));
    }
}

fn extract_attr(tag: &str, name: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let attr = format!("{name}=");
    let start = lower.find(&attr)? + attr.len();
    let quote = tag.as_bytes().get(start).copied()?;
    if quote != b'"' && quote != b'\'' {
        return None;
    }
    let rest = &tag[start + 1..];
    let end = rest.find(quote as char)?;
    Some(decode_html_entities(&rest[..end]))
}

fn clean_result_url(url: String) -> Option<String> {
    let trimmed = url.trim();
    let candidate = if let Some(value) = query_param(trimmed, "uddg") {
        percent_decode(&value)
    } else {
        trimmed.to_string()
    };
    if candidate.starts_with("http://") || candidate.starts_with("https://") {
        Some(candidate)
    } else {
        None
    }
}

fn query_param(url: &str, name: &str) -> Option<String> {
    let query = url.split_once('?')?.1;
    for segment in query.split('&') {
        let (key, value) = segment.split_once('=').unwrap_or((segment, ""));
        if key == name {
            return Some(value.to_string());
        }
    }
    None
}

fn looks_like_html(content_type: &str, body: &str) -> bool {
    content_type.to_ascii_lowercase().contains("html")
        || body
            .trim_start()
            .to_ascii_lowercase()
            .starts_with("<!doctype html")
        || body.trim_start().to_ascii_lowercase().starts_with("<html")
}

fn looks_like_pdf(content_type: &str, url: &str) -> bool {
    content_type
        .to_ascii_lowercase()
        .contains("application/pdf")
        || url
            .split(['?', '#'])
            .next()
            .unwrap_or(url)
            .to_ascii_lowercase()
            .ends_with(".pdf")
}

fn extract_pdf_text_pages(bytes: &[u8]) -> AppResult<Vec<Vec<String>>> {
    if bytes.is_empty() {
        return Err(app_error("PDF response was empty"));
    }
    let path = temp_pdf_path();
    fs::write(&path, bytes)?;
    let command = std::env::var(PDF_TEXT_COMMAND_ENV)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "pdftotext".to_string());
    let output = Command::new(&command)
        .arg("-layout")
        .arg(&path)
        .arg("-")
        .output();
    let _ = fs::remove_file(&path);
    let output = output.map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            app_error(format!(
                "`{command}` is required to extract PDF text for web_run.screenshot; install poppler-utils or set {PDF_TEXT_COMMAND_ENV}"
            ))
        } else {
            app_error(format!("failed to run `{command}` for PDF text extraction: {error}"))
        }
    })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(app_error(format!(
            "`{command}` failed to extract PDF text: {}",
            if stderr.is_empty() {
                output.status.to_string()
            } else {
                stderr
            }
        )));
    }
    let text = String::from_utf8_lossy(&output.stdout).to_string();
    Ok(split_pdf_text_pages(&text))
}

fn temp_pdf_path() -> std::path::PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    std::env::temp_dir().join(format!("dscode-web-run-{}-{nanos}.pdf", std::process::id()))
}

fn split_pdf_text_pages(text: &str) -> Vec<Vec<String>> {
    text.split('\x0C')
        .map(|page| {
            page.lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .collect()
}

fn strip_html_to_text(input: &str) -> String {
    let mut output = String::new();
    let mut in_tag = false;
    let mut last_space = true;
    for ch in input.chars() {
        match ch {
            '<' => {
                in_tag = true;
                if !last_space {
                    output.push(' ');
                    last_space = true;
                }
            }
            '>' => in_tag = false,
            _ if in_tag => {}
            ch if ch.is_whitespace() => {
                if !last_space {
                    output.push(' ');
                    last_space = true;
                }
            }
            ch => {
                output.push(ch);
                last_space = false;
            }
        }
    }
    decode_html_entities(output.trim())
}

fn decode_html_entities(input: &str) -> String {
    input
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&nbsp;", " ")
}

fn url_encode(input: &str) -> String {
    let mut output = String::new();
    for byte in input.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                output.push(byte as char);
            }
            b' ' => output.push('+'),
            _ => output.push_str(&format!("%{byte:02X}")),
        }
    }
    output
}

fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0usize;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            if let Ok(hex) = std::str::from_utf8(&bytes[index + 1..index + 3]) {
                if let Ok(value) = u8::from_str_radix(hex, 16) {
                    output.push(value);
                    index += 3;
                    continue;
                }
            }
        }
        output.push(if bytes[index] == b'+' {
            b' '
        } else {
            bytes[index]
        });
        index += 1;
    }
    String::from_utf8_lossy(&output).to_string()
}

fn parse_usize_arg(input: &ToolInput, key: &str, default: usize) -> usize {
    input
        .get(key)
        .and_then(|value| value.trim().parse::<usize>().ok())
        .unwrap_or(default)
}

fn parse_u64_arg(input: &ToolInput, key: &str, default: u64) -> u64 {
    input
        .get(key)
        .and_then(|value| value.trim().parse::<u64>().ok())
        .unwrap_or(default)
}

fn sanitize_meta(value: &str) -> String {
    value.replace(['\n', '\r'], " ")
}

fn env_flag(name: &str) -> bool {
    matches!(
        std::env::var(name).ok().as_deref(),
        Some("1") | Some("true") | Some("TRUE")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::sync::{Mutex, OnceLock};
    use std::thread;

    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(())).lock().unwrap()
    }

    fn unique_tmp(label: &str) -> std::path::PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("deepseek-{label}-{}-{nanos}", std::process::id()))
    }

    fn serve_once(body: &'static str, content_type: &'static str) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 1024];
            let _ = stream.read(&mut buf);
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(response.as_bytes()).unwrap();
        });
        format!("http://{addr}/")
    }

    #[test]
    fn fetch_url_reads_local_text_when_explicitly_allowed() {
        let _guard = env_lock();
        std::env::set_var("DSCODE_ALLOW_LOCAL_FETCH", "1");
        let url = serve_once("hello from web\n", "text/plain");
        let output = FetchUrlTool
            .execute(
                ToolInput::new()
                    .with_arg("url", url)
                    .with_arg("format", "raw"),
            )
            .unwrap();
        std::env::remove_var("DSCODE_ALLOW_LOCAL_FETCH");

        assert!(output.summary.contains("meta.status=200"));
        assert!(output.summary.contains("hello from web"));
    }

    #[test]
    fn fetch_url_blocks_localhost_by_default() {
        let _guard = env_lock();
        std::env::remove_var("DSCODE_ALLOW_LOCAL_FETCH");
        let error = FetchUrlTool
            .execute(ToolInput::new().with_arg("url", "http://127.0.0.1:9/"))
            .unwrap_err();
        assert!(error.to_string().contains("blocked local or private host"));
    }

    #[test]
    fn fetch_url_respects_network_policy_deny() {
        let _guard = env_lock();
        std::env::set_var("DSCODE_ALLOW_LOCAL_FETCH", "1");
        std::env::set_var("DSCODE_NETWORK_DENY", "127.0.0.1");
        std::env::set_var("DSCODE_NETWORK_AUDIT", "false");
        let error = FetchUrlTool
            .execute(ToolInput::new().with_arg("url", "http://127.0.0.1:9/"))
            .unwrap_err();
        std::env::remove_var("DSCODE_ALLOW_LOCAL_FETCH");
        std::env::remove_var("DSCODE_NETWORK_DENY");
        std::env::remove_var("DSCODE_NETWORK_AUDIT");

        assert!(error.to_string().contains("network policy denied host"));
    }

    #[test]
    fn fetch_url_prompt_policy_requires_approval_unless_marked() {
        let _guard = env_lock();
        std::env::set_var("DSCODE_ALLOW_LOCAL_FETCH", "1");
        std::env::set_var("DSCODE_NETWORK_DEFAULT", "prompt");
        std::env::set_var("DSCODE_NETWORK_AUDIT", "false");
        let denied = FetchUrlTool
            .execute(ToolInput::new().with_arg("url", "http://127.0.0.1:9/"))
            .unwrap_err();
        assert!(denied.to_string().contains("requires approval"));

        let url = serve_once("approved prompt fetch\n", "text/plain");
        let output = FetchUrlTool
            .execute(
                ToolInput::new()
                    .with_arg("url", url)
                    .with_arg("format", "raw")
                    .with_arg(NETWORK_APPROVED_ARG, "true"),
            )
            .unwrap();
        std::env::remove_var("DSCODE_ALLOW_LOCAL_FETCH");
        std::env::remove_var("DSCODE_NETWORK_DEFAULT");
        std::env::remove_var("DSCODE_NETWORK_AUDIT");

        assert!(output.summary.contains("approved prompt fetch"));
    }

    #[test]
    fn fetch_url_appends_network_audit_line() {
        let _guard = env_lock();
        let audit_path = unique_tmp("web-audit").join("network.log");
        std::env::set_var("DSCODE_ALLOW_LOCAL_FETCH", "1");
        std::env::set_var("DSCODE_NETWORK_AUDIT", "true");
        std::env::set_var(
            "DSCODE_NETWORK_AUDIT_PATH",
            audit_path.display().to_string(),
        );
        let url = serve_once("audited fetch\n", "text/plain");
        let output = FetchUrlTool
            .execute(
                ToolInput::new()
                    .with_arg("url", url)
                    .with_arg("format", "raw"),
            )
            .unwrap();
        std::env::remove_var("DSCODE_ALLOW_LOCAL_FETCH");
        std::env::remove_var("DSCODE_NETWORK_AUDIT");
        std::env::remove_var("DSCODE_NETWORK_AUDIT_PATH");

        assert!(output.summary.contains("audited fetch"));
        let audit = std::fs::read_to_string(&audit_path).unwrap();
        assert!(audit.contains(" network 127.0.0.1 web_fetch allow\n"));

        if let Some(root) = audit_path.parent().and_then(|path| path.parent()) {
            let _ = std::fs::remove_dir_all(root);
        }
    }

    #[test]
    fn web_search_parses_anchor_results_from_configured_endpoint() {
        let _guard = env_lock();
        std::env::set_var("DSCODE_ALLOW_LOCAL_FETCH", "1");
        let url = serve_once(
            r#"<html><body>
                <a class="result__a" href="https://example.com/alpha">Alpha &amp; Result</a>
                <div class="result__snippet">First useful snippet.</div>
                <a class="result__a" href="https://example.com/beta">Beta Result</a>
                <div class="result__snippet">Second useful snippet.</div>
            </body></html>"#,
            "text/html",
        );
        std::env::set_var(
            "DSCODE_WEB_SEARCH_URL_TEMPLATE",
            format!("{url}?q={{query}}"),
        );
        let output = WebSearchTool
            .execute(ToolInput::new().with_arg("query", "alpha beta"))
            .unwrap();
        std::env::remove_var("DSCODE_WEB_SEARCH_URL_TEMPLATE");
        std::env::remove_var("DSCODE_ALLOW_LOCAL_FETCH");

        assert!(output.summary.contains("meta.count=2"));
        assert!(output.summary.contains("Alpha & Result"));
        assert!(output.summary.contains("https://example.com/alpha"));
        assert!(output.summary.contains("snippet: First useful snippet."));
    }

    #[test]
    fn web_search_accepts_search_query_array_alias() {
        let _guard = env_lock();
        std::env::set_var("DSCODE_ALLOW_LOCAL_FETCH", "1");
        let url = serve_once(
            r#"<a href="https://example.com/only">Only result</a>"#,
            "text/html",
        );
        std::env::set_var(
            "DSCODE_WEB_SEARCH_URL_TEMPLATE",
            format!("{url}?q={{query}}"),
        );
        let output = WebSearchTool
            .execute(
                ToolInput::new()
                    .with_arg("search_query", r#"[{"q":"only result","max_results":1}]"#),
            )
            .unwrap();
        std::env::remove_var("DSCODE_WEB_SEARCH_URL_TEMPLATE");
        std::env::remove_var("DSCODE_ALLOW_LOCAL_FETCH");

        assert!(output.summary.contains("meta.query=only result"));
        assert!(output.summary.contains("meta.count=1"));
    }

    #[test]
    fn finance_parses_quote_from_configured_endpoint() {
        let _guard = env_lock();
        std::env::set_var("DSCODE_ALLOW_LOCAL_FETCH", "1");
        let url = serve_once(
            r#"{"quoteResponse":{"result":[{"symbol":"AAPL","shortName":"Apple Inc.","regularMarketPrice":190.12,"regularMarketChange":1.25,"regularMarketChangePercent":0.66,"currency":"USD","fullExchangeName":"NasdaqGS","marketState":"REGULAR"}]}}"#,
            "application/json",
        );
        std::env::set_var("DSCODE_FINANCE_URL_TEMPLATE", format!("{url}?s={{ticker}}"));
        let output = FinanceTool
            .execute(ToolInput::new().with_arg("ticker", "aapl"))
            .unwrap();
        std::env::remove_var("DSCODE_FINANCE_URL_TEMPLATE");
        std::env::remove_var("DSCODE_ALLOW_LOCAL_FETCH");

        assert!(output.summary.contains("meta.ticker=AAPL"));
        assert!(output.summary.contains("symbol: AAPL"));
        assert!(output.summary.contains("name: Apple Inc."));
        assert!(output.summary.contains("price: 190.12 USD"));
        assert!(output.summary.contains("change: 1.25 (0.66%)"));
        assert!(output.summary.contains("exchange: NasdaqGS"));
    }

    #[test]
    fn finance_converts_crypto_hint_to_usd_pair() {
        let _guard = env_lock();
        std::env::set_var("DSCODE_ALLOW_LOCAL_FETCH", "1");
        let url = serve_once(
            r#"{"quoteResponse":{"result":[{"symbol":"BTC-USD","shortName":"Bitcoin USD","regularMarketPrice":100000,"currency":"USD"}]}}"#,
            "application/json",
        );
        std::env::set_var("DSCODE_FINANCE_URL_TEMPLATE", format!("{url}?s={{ticker}}"));
        let output = FinanceTool
            .execute(
                ToolInput::new()
                    .with_arg("symbol", "btc")
                    .with_arg("type", "crypto"),
            )
            .unwrap();
        std::env::remove_var("DSCODE_FINANCE_URL_TEMPLATE");
        std::env::remove_var("DSCODE_ALLOW_LOCAL_FETCH");

        assert!(output.summary.contains("meta.ticker=BTC-USD"));
        assert!(output.summary.contains("symbol: BTC-USD"));
        assert!(output.summary.contains("price: 100000 USD"));
    }

    #[test]
    fn finance_reports_unknown_ticker() {
        let _guard = env_lock();
        std::env::set_var("DSCODE_ALLOW_LOCAL_FETCH", "1");
        let url = serve_once(r#"{"quoteResponse":{"result":[]}}"#, "application/json");
        std::env::set_var("DSCODE_FINANCE_URL_TEMPLATE", format!("{url}?s={{ticker}}"));
        let error = FinanceTool
            .execute(ToolInput::new().with_arg("ticker", "NOPE"))
            .unwrap_err();
        std::env::remove_var("DSCODE_FINANCE_URL_TEMPLATE");
        std::env::remove_var("DSCODE_ALLOW_LOCAL_FETCH");

        assert!(error.to_string().contains("Unknown finance ticker"));
    }

    #[test]
    fn finance_rejects_unsafe_symbol() {
        let error = FinanceTool
            .execute(ToolInput::new().with_arg("ticker", "../AAPL"))
            .unwrap_err();

        assert!(error.to_string().contains("unsupported characters"));
    }

    #[test]
    fn web_run_search_query_delegates_to_web_search() {
        let _guard = env_lock();
        std::env::set_var("DSCODE_ALLOW_LOCAL_FETCH", "1");
        let url = serve_once(
            r#"<a href="https://example.com/web-run">Web Run Result</a>"#,
            "text/html",
        );
        std::env::set_var(
            "DSCODE_WEB_SEARCH_URL_TEMPLATE",
            format!("{url}?q={{query}}"),
        );
        let output = WebRunTool
            .execute(
                ToolInput::new().with_arg("search_query", r#"[{"q":"web run","max_results":1}]"#),
            )
            .unwrap();
        std::env::remove_var("DSCODE_WEB_SEARCH_URL_TEMPLATE");
        std::env::remove_var("DSCODE_ALLOW_LOCAL_FETCH");

        assert!(output.summary.contains("meta.tool=web_run"));
        assert!(output.summary.contains("meta.supported_actions=1"));
        assert!(output.summary.contains("--- web_run search_query[0] ---"));
        assert!(output.summary.contains("Web Run Result"));
    }

    #[test]
    fn web_run_open_fetches_direct_url() {
        let _guard = env_lock();
        std::env::set_var("DSCODE_ALLOW_LOCAL_FETCH", "1");
        let url = serve_once("direct open body\n", "text/plain");
        let output = WebRunTool
            .execute(
                ToolInput::new()
                    .with_arg("open", format!(r#"[{{"ref_id":"{url}","format":"raw"}}]"#)),
            )
            .unwrap();
        std::env::remove_var("DSCODE_ALLOW_LOCAL_FETCH");

        assert!(output.summary.contains("--- web_run open[0] ---"));
        assert!(output.summary.contains("direct open body"));
    }

    #[test]
    fn web_run_open_honors_lineno_and_response_length() {
        let _guard = env_lock();
        std::env::set_var("DSCODE_ALLOW_LOCAL_FETCH", "1");
        let body = (1..=120)
            .map(|line| format!("line {line}"))
            .collect::<Vec<_>>()
            .join("\n");
        let url = serve_once(Box::leak(body.into_boxed_str()), "text/plain");
        let output = WebRunTool
            .execute(
                ToolInput::new()
                    .with_arg("response_length", "short")
                    .with_arg("open", format!(r#"[{{"ref_id":"{url}","lineno":41}}]"#)),
            )
            .unwrap();
        std::env::remove_var("DSCODE_ALLOW_LOCAL_FETCH");

        assert!(output.summary.contains("meta.line_start=41"));
        assert!(output.summary.contains("meta.line_end=80"));
        assert!(output.summary.contains("meta.total_lines=120"));
        assert!(output.summary.contains("line 41"));
        assert!(!output.summary.contains("line 40"));
    }

    #[test]
    fn web_run_open_window_keeps_full_page_cached_for_find() {
        let _guard = env_lock();
        std::env::set_var("DSCODE_ALLOW_LOCAL_FETCH", "1");
        let body = (1..=95)
            .map(|line| {
                if line == 95 {
                    "needle on final line".to_string()
                } else {
                    format!("line {line}")
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        let url = serve_once(Box::leak(body.into_boxed_str()), "text/plain");
        let opened = WebRunTool
            .execute(
                ToolInput::new()
                    .with_arg("response_length", "short")
                    .with_arg("open", format!(r#"[{{"ref_id":"{url}","lineno":1}}]"#)),
            )
            .unwrap();
        assert!(opened.summary.contains("meta.line_end=40"));
        assert!(!opened.summary.contains("needle on final line"));

        let found = WebRunTool
            .execute(
                ToolInput::new().with_arg("find", r#"[{"ref_id":"open0","pattern":"needle"}]"#),
            )
            .unwrap();
        std::env::remove_var("DSCODE_ALLOW_LOCAL_FETCH");

        assert!(found.summary.contains("meta.source=cache"));
        assert!(found.summary.contains("needle on final line"));
    }

    #[test]
    fn web_run_open_exposes_links_and_click_fetches_target() {
        let _guard = env_lock();
        std::env::set_var("DSCODE_ALLOW_LOCAL_FETCH", "1");
        let target_url = serve_once("clicked target body\nneedle after click\n", "text/plain");
        let page_url = serve_once(
            Box::leak(
                format!(r#"<html><body><a href="{target_url}">Open target</a></body></html>"#)
                    .into_boxed_str(),
            ),
            "text/html",
        );

        let opened = WebRunTool
            .execute(ToolInput::new().with_arg("open", format!(r#"[{{"ref_id":"{page_url}"}}]"#)))
            .unwrap();
        assert!(opened.summary.contains("links:"));
        assert!(opened.summary.contains("[1] Open target ->"));

        let mut network = NetworkConfig::default();
        network.default = "prompt".to_string();
        let approval_target = network_permission_target_for_tool(
            "web_run",
            &ToolInput::new().with_arg("click", r#"[{"ref_id":"open0","id":1}]"#),
            &network,
        )
        .expect("click target should require network approval");
        assert!(approval_target.contains("127.0.0.1"));

        std::env::set_var("DSCODE_NETWORK_DEFAULT", "prompt");
        std::env::set_var("DSCODE_NETWORK_AUDIT", "false");
        let denied = WebRunTool
            .execute(ToolInput::new().with_arg("click", r#"[{"ref_id":"open0","id":1}]"#))
            .unwrap_err();
        assert!(denied.to_string().contains("requires approval"));

        let clicked = WebRunTool
            .execute(
                ToolInput::new()
                    .with_arg("click", r#"[{"ref_id":"open0","id":1}]"#)
                    .with_arg(NETWORK_APPROVED_ARG, "true"),
            )
            .unwrap();
        assert!(clicked.summary.contains("--- web_run click[0] ---"));
        assert!(clicked.summary.contains("meta.clicked_ref=open0"));
        assert!(clicked.summary.contains("clicked target body"));
        assert!(clicked
            .summary
            .contains("web_run_ref: click0 -> cached page"));

        let found = WebRunTool
            .execute(
                ToolInput::new().with_arg("find", r#"[{"ref_id":"click0","pattern":"needle"}]"#),
            )
            .unwrap();
        std::env::remove_var("DSCODE_ALLOW_LOCAL_FETCH");
        std::env::remove_var("DSCODE_NETWORK_DEFAULT");
        std::env::remove_var("DSCODE_NETWORK_AUDIT");

        assert!(found.summary.contains("meta.source=cache"));
        assert!(found.summary.contains("needle after click"));
    }

    #[test]
    fn web_run_click_honors_lineno_window() {
        let _guard = env_lock();
        std::env::set_var("DSCODE_ALLOW_LOCAL_FETCH", "1");
        let target_body = (1..=60)
            .map(|line| format!("target line {line}"))
            .collect::<Vec<_>>()
            .join("\n");
        let target_url = serve_once(Box::leak(target_body.into_boxed_str()), "text/plain");
        let page_url = serve_once(
            Box::leak(
                format!(r#"<html><body><a href="{target_url}">Open target</a></body></html>"#)
                    .into_boxed_str(),
            ),
            "text/html",
        );

        WebRunTool
            .execute(ToolInput::new().with_arg("open", format!(r#"[{{"ref_id":"{page_url}"}}]"#)))
            .unwrap();
        let clicked = WebRunTool
            .execute(
                ToolInput::new()
                    .with_arg("response_length", "short")
                    .with_arg("click", r#"[{"ref_id":"open0","id":1,"lineno":41}]"#),
            )
            .unwrap();
        std::env::remove_var("DSCODE_ALLOW_LOCAL_FETCH");

        assert!(clicked.summary.contains("meta.line_start=41"));
        assert!(clicked.summary.contains("meta.line_end=60"));
        assert!(clicked.summary.contains("target line 41"));
        assert!(!clicked.summary.contains("target line 40"));
    }

    #[test]
    fn web_run_click_rejects_missing_link_id() {
        let _guard = env_lock();
        std::env::set_var("DSCODE_ALLOW_LOCAL_FETCH", "1");
        let page_url = serve_once(
            r#"<html><body><a href="https://example.com/ok">Only link</a></body></html>"#,
            "text/html",
        );
        WebRunTool
            .execute(ToolInput::new().with_arg("open", format!(r#"[{{"ref_id":"{page_url}"}}]"#)))
            .unwrap();

        let error = WebRunTool
            .execute(ToolInput::new().with_arg("click", r#"[{"ref_id":"open0","id":2}]"#))
            .unwrap_err();
        std::env::remove_var("DSCODE_ALLOW_LOCAL_FETCH");

        assert!(error.to_string().contains("link id 2 not found"));
    }

    #[test]
    fn normalize_link_url_resolves_relative_links() {
        assert_eq!(
            normalize_link_url("https://example.com/docs/page.html", "../img/logo.png").as_deref(),
            Some("https://example.com/img/logo.png")
        );
        assert_eq!(
            normalize_link_url("https://example.com/docs/page.html?x=1", "/guide/start").as_deref(),
            Some("https://example.com/guide/start")
        );
        assert_eq!(
            normalize_link_url("https://example.com/docs/page.html", "?next=1").as_deref(),
            Some("https://example.com/docs/page.html?next=1")
        );
        assert!(
            normalize_link_url("https://example.com/docs/page.html", "javascript:alert(1)")
                .is_none()
        );
    }

    #[test]
    fn split_pdf_text_pages_uses_form_feed_boundaries() {
        assert_eq!(
            split_pdf_text_pages("  first page\n\n\x0Csecond page\n  more\n"),
            vec![
                vec!["first page".to_string()],
                vec!["second page".to_string(), "more".to_string()]
            ]
        );
    }

    #[test]
    fn web_run_screenshot_returns_cached_pdf_page() {
        {
            let mut state = web_run_state().lock().unwrap();
            state.urls.insert(
                "pdf-screenshot-ref".to_string(),
                "https://example.com/file.pdf".to_string(),
            );
            state.pages.insert(
                "pdf-screenshot-ref".to_string(),
                WebRunCachedPage {
                    text: "first page".to_string(),
                    links: Vec::new(),
                    pdf_pages: vec![
                        vec!["first page".to_string()],
                        vec!["second page".to_string(), "needle".to_string()],
                    ],
                },
            );
        }

        let output = WebRunTool
            .execute(ToolInput::new().with_arg(
                "screenshot",
                r#"[{"ref_id":"pdf-screenshot-ref","pageno":1}]"#,
            ))
            .unwrap();

        assert!(output.summary.contains("meta.supported_actions=1"));
        assert!(output.summary.contains("--- web_run screenshot[0] ---"));
        assert!(output.summary.contains("meta.pageno=1"));
        assert!(output.summary.contains("meta.total_pages=2"));
        assert!(output.summary.contains("second page\nneedle"));
    }

    #[test]
    fn web_run_screenshot_rejects_non_pdf_page() {
        {
            let mut state = web_run_state().lock().unwrap();
            state.urls.insert(
                "html-screenshot-ref".to_string(),
                "https://example.com/page.html".to_string(),
            );
            state.pages.insert(
                "html-screenshot-ref".to_string(),
                WebRunCachedPage {
                    text: "html page".to_string(),
                    links: Vec::new(),
                    pdf_pages: Vec::new(),
                },
            );
        }

        let error = WebRunTool
            .execute(ToolInput::new().with_arg(
                "screenshot",
                r#"[{"ref_id":"html-screenshot-ref","pageno":0}]"#,
            ))
            .unwrap_err();

        assert!(error.to_string().contains("cached PDF pages"));
    }

    #[test]
    fn web_run_find_matches_direct_url() {
        let _guard = env_lock();
        std::env::set_var("DSCODE_ALLOW_LOCAL_FETCH", "1");
        let url = serve_once("alpha\nneedle in a useful line\nomega\n", "text/plain");
        let output = WebRunTool
            .execute(ToolInput::new().with_arg(
                "find",
                format!(r#"[{{"ref_id":"{url}","pattern":"needle"}}]"#),
            ))
            .unwrap();
        std::env::remove_var("DSCODE_ALLOW_LOCAL_FETCH");

        assert!(output.summary.contains("meta.count=1"));
        assert!(output.summary.contains("needle in a useful line"));
    }

    #[test]
    fn web_run_finance_delegates_quote_lookup() {
        let _guard = env_lock();
        std::env::set_var("DSCODE_ALLOW_LOCAL_FETCH", "1");
        let url = serve_once(
            r#"{"quoteResponse":{"result":[{"symbol":"AAPL","shortName":"Apple Inc.","regularMarketPrice":190.12,"currency":"USD"}]}}"#,
            "application/json",
        );
        std::env::set_var("DSCODE_FINANCE_URL_TEMPLATE", format!("{url}?s={{ticker}}"));
        let output = WebRunTool
            .execute(ToolInput::new().with_arg("finance", r#"[{"ticker":"aapl","type":"equity"}]"#))
            .unwrap();
        std::env::remove_var("DSCODE_FINANCE_URL_TEMPLATE");
        std::env::remove_var("DSCODE_ALLOW_LOCAL_FETCH");

        assert!(output.summary.contains("--- web_run finance[0] ---"));
        assert!(output.summary.contains("symbol: AAPL"));
        assert!(output.summary.contains("price: 190.12 USD"));
    }

    #[test]
    fn web_run_image_query_reads_configured_json_endpoint() {
        let _guard = env_lock();
        std::env::set_var("DSCODE_ALLOW_LOCAL_FETCH", "1");
        let url = serve_once(
            r#"{"results":[{"image":"https://img.example/alpha.png","thumbnail":"https://thumb.example/alpha.jpg","title":"Alpha image","url":"https://example.com/page","source":"Example","width":640,"height":480}]}"#,
            "application/json",
        );
        std::env::set_var(
            "DSCODE_IMAGE_SEARCH_URL_TEMPLATE",
            format!("{url}?q={{query}}"),
        );
        let output = WebRunTool
            .execute(
                ToolInput::new()
                    .with_arg("image_query", r#"[{"q":"alpha image","max_results":1}]"#),
            )
            .unwrap();
        std::env::remove_var("DSCODE_IMAGE_SEARCH_URL_TEMPLATE");
        std::env::remove_var("DSCODE_ALLOW_LOCAL_FETCH");

        assert!(output.summary.contains("meta.supported_actions=1"));
        assert!(output.summary.contains("--- web_run image_query[0] ---"));
        assert!(output.summary.contains("meta.source=custom"));
        assert!(output.summary.contains("Alpha image"));
        assert!(output
            .summary
            .contains("image: https://img.example/alpha.png"));
        assert!(output
            .summary
            .contains("thumbnail: https://thumb.example/alpha.jpg"));
        assert!(output.summary.contains("size: 640x480"));
        assert!(!output.summary.contains("unsupported_actions=image_query"));
    }

    #[test]
    fn web_run_image_query_filters_domains() {
        let _guard = env_lock();
        std::env::set_var("DSCODE_ALLOW_LOCAL_FETCH", "1");
        let url = serve_once(
            r#"{"results":[{"image":"https://img.example/keep.png","title":"Keep","url":"https://keep.example/page"},{"image":"https://img.example/drop.png","title":"Drop","url":"https://drop.example/page"}]}"#,
            "application/json",
        );
        std::env::set_var(
            "DSCODE_IMAGE_SEARCH_URL_TEMPLATE",
            format!("{url}?q={{query}}"),
        );
        let output = WebRunTool
            .execute(ToolInput::new().with_arg(
                "image_query",
                r#"[{"q":"domain filter","domains":["keep.example"],"max_results":5}]"#,
            ))
            .unwrap();
        std::env::remove_var("DSCODE_IMAGE_SEARCH_URL_TEMPLATE");
        std::env::remove_var("DSCODE_ALLOW_LOCAL_FETCH");

        assert!(output.summary.contains("meta.count=1"));
        assert!(output.summary.contains("Keep"));
        assert!(!output.summary.contains("Drop"));
        assert!(output
            .summary
            .contains("Filtered image results by domain list"));
    }

    #[test]
    fn web_run_reports_unsupported_actions() {
        let output = WebRunTool
            .execute(ToolInput::new().with_arg("weather", r#"[{"location":"Boston"}]"#))
            .unwrap();

        assert!(output.summary.contains("meta.unsupported_actions=weather"));
        assert!(output
            .summary
            .contains("Unsupported web_run actions in this slice: weather"));
    }

    #[test]
    fn web_run_open_resolves_search_ref_from_prior_call() {
        let _guard = env_lock();
        std::env::set_var("DSCODE_ALLOW_LOCAL_FETCH", "1");
        let page_url = serve_once("stored search ref body\n", "text/plain");
        let search_url = serve_once(
            Box::leak(format!(r#"<a href="{page_url}">Stored page</a>"#).into_boxed_str()),
            "text/html",
        );
        std::env::set_var(
            "DSCODE_WEB_SEARCH_URL_TEMPLATE",
            format!("{search_url}?q={{query}}"),
        );
        let search = WebRunTool
            .execute(ToolInput::new().with_arg("search_query", r#"[{"q":"stored page"}]"#))
            .unwrap();
        assert!(search.summary.contains("web_run_ref: search0 ->"));

        let opened = WebRunTool
            .execute(ToolInput::new().with_arg("open", r#"[{"ref_id":"search0"}]"#))
            .unwrap();
        std::env::remove_var("DSCODE_WEB_SEARCH_URL_TEMPLATE");
        std::env::remove_var("DSCODE_ALLOW_LOCAL_FETCH");

        assert!(opened.summary.contains("stored search ref body"));
        assert!(opened.summary.contains("web_run_ref: open0 -> cached page"));
    }

    #[test]
    fn web_run_find_uses_cached_open_ref() {
        let _guard = env_lock();
        std::env::set_var("DSCODE_ALLOW_LOCAL_FETCH", "1");
        let url = serve_once("alpha\ncached needle line\nomega\n", "text/plain");
        let opened = WebRunTool
            .execute(
                ToolInput::new()
                    .with_arg("open", format!(r#"[{{"ref_id":"{url}","format":"raw"}}]"#)),
            )
            .unwrap();
        assert!(opened.summary.contains("web_run_ref: open0 -> cached page"));

        let found = WebRunTool
            .execute(
                ToolInput::new().with_arg("find", r#"[{"ref_id":"open0","pattern":"needle"}]"#),
            )
            .unwrap();
        std::env::remove_var("DSCODE_ALLOW_LOCAL_FETCH");

        assert!(found.summary.contains("meta.source=cache"));
        assert!(found.summary.contains("cached needle line"));
    }

    #[test]
    fn parse_duckduckgo_redirect_result_url() {
        let html = r#"<a class="result__a" href="/l/?uddg=https%3A%2F%2Fexample.com%2Fdoc&amp;rut=x">Doc</a>"#;
        let results = parse_search_results(html, 5);
        assert_eq!(results[0].url, "https://example.com/doc");
    }
}
