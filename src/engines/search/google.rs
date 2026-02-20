use std::{
    net::IpAddr,
    str::FromStr,
    sync::LazyLock,
    time::{Duration, Instant},
};

use eyre::eyre;
use parking_lot::Mutex;
use rand::Rng;
use scraper::{ElementRef, Selector};
use tracing::warn;
use url::Url;

use crate::{
    engines::{
        EngineImageResult, EngineImagesResponse, EngineResponse, RequestResponse, SearchQuery,
        CLIENT,
    },
    parse::{parse_html_response_with_opts, ParseOpts, QueryMethod},
};

// --- Google GSA Client ---

/// Dedicated HTTP client for Google search requests.
///
/// The shared `CLIENT` bakes in a Firefox User-Agent as a default header.
/// wreq's header merging would produce duplicate UA values if we tried to
/// override per-request, so we build a separate client with no default UA.
static GOOGLE_CLIENT: LazyLock<wreq::Client> = LazyLock::new(|| {
    wreq::Client::builder()
        .local_address(IpAddr::from_str("0.0.0.0").unwrap())
        .emulation(wreq_util::Emulation::Safari18_2)
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap()
});

// --- GSA User-Agent Pool ---

/// User-Agent strings for the Google Search App on iOS.
///
/// Covers iOS 17-18 and recent GSA versions. One is picked at random per
/// request to reduce fingerprinting surface.
static GSA_USER_AGENTS: &[&str] = &[
    "Mozilla/5.0 (iPhone14,6; CPU iPhone OS 17_7_2 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) GSA/344.0.695551749 Mobile/15E148 Safari/604.1",
    "Mozilla/5.0 (iPhone16,2; CPU iPhone OS 18_1_1 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) GSA/345.1.700380567 Mobile/15E148 Safari/604.1",
    "Mozilla/5.0 (iPhone15,3; CPU iPhone OS 17_6_1 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) GSA/343.0.694165092 Mobile/15E148 Safari/604.1",
    "Mozilla/5.0 (iPhone16,1; CPU iPhone OS 18_0_1 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) GSA/344.0.695551749 Mobile/15E148 Safari/604.1",
    "Mozilla/5.0 (iPhone14,7; CPU iPhone OS 17_5 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) GSA/342.0.693056097 Mobile/15E148 Safari/604.1",
    "Mozilla/5.0 (iPhone15,4; CPU iPhone OS 18_1 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) GSA/345.1.700380567 Mobile/15E148 Safari/604.1",
    "Mozilla/5.0 (iPhone16,2; CPU iPhone OS 17_7 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) GSA/343.0.694165092 Mobile/15E148 Safari/604.1",
    "Mozilla/5.0 (iPhone15,2; CPU iPhone OS 18_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) GSA/344.0.695551749 Mobile/15E148 Safari/604.1",
];

// --- Arc ID ---

/// Character set for arc_id random generation.
const ARC_CHARSET: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789_-";

/// Length of the random portion of the arc_id.
const ARC_ID_LEN: usize = 23;

/// How long an arc_id stays valid before rotation.
const ARC_ID_TTL: Duration = Duration::from_secs(3600);

/// Cached arc_id and its creation time. Rotates hourly.
static ARC_ID: LazyLock<Mutex<(String, Instant)>> = LazyLock::new(|| {
    Mutex::new((generate_arc_id(), Instant::now()))
});

/// Generates a fresh random arc_id string.
fn generate_arc_id() -> String {
    let mut rng = rand::rng();
    (0..ARC_ID_LEN)
        .map(|_| {
            let idx = rng.random_range(0..ARC_CHARSET.len());
            ARC_CHARSET[idx] as char
        })
        .collect()
}

/// Returns the current arc_id, rotating it if stale.
///
/// The full `async` parameter value for the request is built from this id.
fn get_async_param() -> String {
    let mut guard = ARC_ID.lock();
    if guard.1.elapsed() > ARC_ID_TTL {
        *guard = (generate_arc_id(), Instant::now());
    }
    let arc_id = &guard.0;
    format!("arc_id:srp_{arc_id}_100,use_ac:true,_fmt:prog")
}

// --- Search ---

pub async fn request(search: &SearchQuery) -> eyre::Result<RequestResponse> {
    let url = Url::parse_with_params(
        "https://www.google.com/search",
        &[
            ("q", search.query.as_str()),
            ("hl", "en-US"),
            ("ie", "utf8"),
            ("oe", "utf8"),
            ("filter", "0"),
            ("start", "0"),
            ("asearch", "arc"),
            ("async", &get_async_param()),
        ],
    )
    .unwrap();

    let ua = GSA_USER_AGENTS[rand::rng().random_range(0..GSA_USER_AGENTS.len())];

    Ok(GOOGLE_CLIENT
        .get(url.as_str())
        .header("User-Agent", ua)
        .header("Accept", "*/*")
        // Pre-accept Google's consent screen (EU cookie wall).
        .header("Cookie", "CONSENT=YES+")
        .header("Sec-Fetch-Dest", "empty")
        .header("Sec-Fetch-Mode", "cors")
        .header("Sec-Fetch-Site", "same-origin")
        .into())
}

pub fn parse_response(body: &str) -> eyre::Result<EngineResponse> {
    // Detect Google's CAPTCHA/block redirect (sorry.google.com).
    if body.contains("sorry.google.com") || body.contains("/sorry/index") {
        return Err(eyre!("google is blocking requests from this IP (sorry/captcha page)"));
    }

    parse_html_response_with_opts(
        body,
        ParseOpts::new()
            .result("div.MjjYud")
            .title("div[role='link']")
            .href(QueryMethod::Manual(Box::new(|el: &ElementRef| {
                let selector = Selector::parse("a[href]").unwrap();
                let url = el
                    .select(&selector)
                    .next()
                    .and_then(|n| n.value().attr("href"))
                    .unwrap_or_default();
                clean_url(url)
            })))
            .description("div[data-sncf='1']"),
    )
}

// --- Autocomplete ---

pub fn request_autocomplete(query: &str) -> wreq::RequestBuilder {
    CLIENT.get(
        Url::parse_with_params(
            "https://suggestqueries.google.com/complete/search",
            &[
                ("output", "firefox"),
                ("client", "firefox"),
                ("hl", "US-en"),
                ("q", query),
            ],
        )
        .unwrap()
        .as_str(),
    )
}

pub fn parse_autocomplete_response(body: &str) -> eyre::Result<Vec<String>> {
    let res = serde_json::from_str::<Vec<serde_json::Value>>(body)?;
    Ok(res
        .into_iter()
        .nth(1)
        .unwrap_or_default()
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|v| v.as_str().unwrap_or_default().to_string())
        .collect())
}

// --- Images ---

pub fn request_images(query: &str) -> wreq::RequestBuilder {
    // Google also has a json api for images but it gives us fewer results.
    CLIENT.get(
        Url::parse_with_params(
            "https://www.google.com/search",
            &[("q", query), ("udm", "2"), ("prmd", "ivsnmbtz")],
        )
        .unwrap()
        .as_str(),
    )
}

pub fn parse_images_response(body: &str) -> eyre::Result<EngineImagesResponse> {
    // We can't just scrape the html because it won't give us the image sources,
    // so we have to scrape their internal json.

    // Iterate through every script until we find something that matches our regex.
    let internal_json_regex =
        regex::Regex::new(r#"(?:\(function\(\)\{google\.jl=\{.+?)var \w=(\{".+?\});"#)?;
    let mut internal_json = None;
    let dom = scraper::Html::parse_document(body);
    for script in dom.select(&Selector::parse("script").unwrap()) {
        let script = script.inner_html();
        if let Some(captures) = internal_json_regex.captures(&script).and_then(|c| c.get(1)) {
            internal_json = Some(captures.as_str().to_string());
            break;
        }
    }

    let internal_json =
        internal_json.ok_or_else(|| eyre!("couldn't get internal json for google images"))?;
    let internal_json: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(&internal_json)?;

    let mut image_results = Vec::new();
    for element_json in internal_json.values() {
        // The internal json uses arrays instead of maps, which makes it kinda hard to
        // use and also probably pretty unstable.

        let Some(element_json) = element_json
            .as_array()
            .and_then(|a| a.get(1))
            .and_then(|v| v.as_array())
        else {
            continue;
        };

        let Some((image_url, width, height)) = element_json
            .get(3)
            .and_then(|v| serde_json::from_value(v.clone()).ok())
        else {
            warn!("couldn't get image data from google images json");
            continue;
        };

        // This is probably pretty brittle, hopefully Google doesn't break it any
        // time soon.
        let Some(page) = element_json
            .get(9)
            .and_then(|v| v.as_object())
            .and_then(|o| o.get("2003"))
            .and_then(|v| v.as_array())
        else {
            warn!("couldn't get page data from google images json");
            continue;
        };
        let Some(page_url) = page.get(2).and_then(|v| v.as_str()).map(|s| s.to_string()) else {
            warn!("couldn't get page url from google images json");
            continue;
        };
        let Some(title) = page.get(3).and_then(|v| v.as_str()).map(|s| s.to_string()) else {
            warn!("couldn't get page title from google images json");
            continue;
        };

        image_results.push(EngineImageResult {
            image_url,
            page_url,
            title,
            width,
            height,
        });
    }

    Ok(EngineImagesResponse { image_results })
}

// --- Helpers ---

/// Extracts the real destination URL from a Google redirect wrapper.
///
/// Google wraps result URLs in `/url?q=<encoded>&sa=U&...`. This strips the
/// prefix, splits on the `&sa=U` sentinel, and URL-decodes the remainder.
/// Non-wrapped URLs pass through unchanged.
fn clean_url(url: &str) -> eyre::Result<String> {
    if let Some(remainder) = url.strip_prefix("/url?q=") {
        let cleaned = remainder.split("&sa=U").next().unwrap_or(remainder);
        Ok(urlencoding::decode(cleaned)
            .map_or_else(|_| cleaned.to_string(), |s| s.into_owned()))
    }
    else {
        Ok(url.to_string())
    }
}
