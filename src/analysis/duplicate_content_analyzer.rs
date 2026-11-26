// SiteOne Crawler - DuplicateContentAnalyzer
// (c) Jan Reges <jan.reges@siteone.cz>
//
// Near-duplicate and thin content detection:
//   1. Near-duplicate content: a 64-bit SimHash fingerprint of each page's visible main content
//      (5-word shingles). Pages whose fingerprints are within 4 bits of Hamming distance are
//      grouped together. Groups of 2+ pages are reported with their representative (shortest URL),
//      an approximate similarity, and a per-group recommendation (keep one, 301/noindex the rest,
//      or add canonical). Groups made only of URL variants (same path, different query strings)
//      are additionally flagged — the typical faceted-navigation / ?utm_* / pagination problem.
//   2. Thin content: indexable pages with fewer than 100 words of visible text — pages Google
//      commonly demotes as low-quality.
//
// Deterministic; no network, no LLM, no new dependencies (SimHash + FNV-1a implemented in-crate).

use std::collections::HashMap;
use std::time::Instant;

use crate::analysis::analyzer::Analyzer;
use crate::analysis::base_analyzer::BaseAnalyzer;
use crate::analysis::text_utils;
use crate::components::super_table::SuperTable;
use crate::components::super_table_column::SuperTableColumn;
use crate::output::output::Output;
use crate::result::status::Status;
use crate::result::visited_url::VisitedUrl;
use crate::types::ContentTypeId;
use crate::utils;

const SUPER_TABLE_DUPLICATES: &str = "duplicate-content";

/// Fewer than this many words of visible text = thin content.
pub const THIN_WORD_THRESHOLD: usize = 100;
/// Hamming distance (out of 64 bits) at or below which two pages are near-duplicates.
pub const NEAR_DUP_HAMMING: u32 = 4;
/// Word-level SimHash shingles (n=1) — the standard, most stable configuration for
/// near-duplicate detection (matches Google's original SimHash; permutation-invariant).
pub const SHINGLE_SIZE: usize = 1;
/// Fingerprinting needs at least this many words to be meaningful.
pub const MIN_WORDS_FOR_FINGERPRINT: usize = 8;

/// FNV-1a 64-bit hash (implemented in-crate to avoid a new dependency).
pub fn fnv1a_64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    h
}

/// Alphanumeric tokens (lowercased) of a text.
fn tokens(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect()
}

/// SimHash (64-bit) fingerprint of a token stream (word-level shingles).
/// Returns None when the text has fewer than MIN_WORDS_FOR_FINGERPRINT tokens.
pub fn simhash_fingerprint(text: &str) -> Option<u64> {
    let toks = tokens(text);
    if toks.len() < MIN_WORDS_FOR_FINGERPRINT {
        return None;
    }
    let mut bits: [i64; 64] = [0; 64];
    for window in toks.windows(SHINGLE_SIZE) {
        let shingle = window.join(" ");
        let h = fnv1a_64(shingle.as_bytes());
        for (i, bit) in bits.iter_mut().enumerate() {
            *bit += if (h >> i) & 1 == 1 { 1 } else { -1 };
        }
    }
    let mut fingerprint: u64 = 0;
    for (i, &bit) in bits.iter().enumerate() {
        if bit > 0 {
            fingerprint |= 1u64 << i;
        }
    }
    Some(fingerprint)
}

/// Hamming distance between two 64-bit fingerprints.
pub fn hamming_distance(a: u64, b: u64) -> u32 {
    (a ^ b).count_ones()
}

/// Path (without query) of a URL; the full URL when it cannot be parsed.
fn url_path(url: &str) -> String {
    if let Ok(parsed) = url::Url::parse(url) {
        parsed.path().to_string()
    } else {
        url.to_string()
    }
}

/// True when all URLs of a group share the same path (i.e. the group is only URL variants:
/// different query strings / fragments of the same document).
fn group_is_url_variant(urls: &[String]) -> bool {
    if urls.len() < 2 {
        return false;
    }
    let first = url_path(&urls[0]);
    urls.iter().all(|u| url_path(u) == first)
}

pub struct DuplicateContentAnalyzer {
    base: BaseAnalyzer,
}

impl DuplicateContentAnalyzer {
    pub fn new() -> Self {
        Self {
            base: BaseAnalyzer::new(),
        }
    }

    fn is_html_page(u: &VisitedUrl) -> bool {
        u.content_type == ContentTypeId::Html && u.status_code == 200 && u.is_allowed_for_crawling
    }

    fn get_url_path_and_query(url: &str) -> String {
        if let Ok(parsed) = url::Url::parse(url) {
            let path = parsed.path().to_string();
            if let Some(query) = parsed.query() {
                format!("{}?{}", path, query)
            } else {
                path
            }
        } else {
            url.to_string()
        }
    }
}

impl Default for DuplicateContentAnalyzer {
    fn default() -> Self {
        Self::new()
    }
}

struct PageFingerprint {
    url: String,
    path_query: String,
    words: usize,
    fingerprint: Option<u64>,
}

/// Union-find parent pointers over the fingerprinted pages.
fn union_find(parent: &mut [usize], a: usize, b: usize) {
    let ra = find(parent, a);
    let rb = find(parent, b);
    if ra != rb {
        parent[ra] = rb;
    }
}

fn find(parent: &mut [usize], mut x: usize) -> usize {
    while parent[x] != x {
        parent[x] = parent[parent[x]];
        x = parent[x];
    }
    x
}

impl Analyzer for DuplicateContentAnalyzer {
    fn analyze(&mut self, status: &Status, output: &mut dyn Output) {
        let s = Instant::now();

        let visited = status.get_visited_urls();
        let html_pages: Vec<&VisitedUrl> = visited.iter().filter(|u| Self::is_html_page(u)).collect();

        // ---- Fingerprint every page -------------------------------------------------------
        let mut pages: Vec<PageFingerprint> = Vec::new();
        for u in &html_pages {
            let body = match status.get_url_body_text(&u.uq_id) {
                Some(b) => b,
                None => continue,
            };
            let text = text_utils::visible_main_text(&body);
            pages.push(PageFingerprint {
                url: u.url.clone(),
                path_query: Self::get_url_path_and_query(&u.url),
                words: text.split_whitespace().count(),
                fingerprint: simhash_fingerprint(&text),
            });
        }

        // ---- Group near-duplicates (Hamming <= threshold, union-find) -----------------------
        let n = pages.len();
        let mut parent: Vec<usize> = (0..n).collect();
        for i in 0..n {
            let fi = match pages[i].fingerprint {
                Some(f) => f,
                None => continue,
            };
            for (j, pagej) in pages.iter().enumerate().skip(i + 1) {
                let fj = match pagej.fingerprint {
                    Some(f) => f,
                    None => continue,
                };
                if hamming_distance(fi, fj) <= NEAR_DUP_HAMMING {
                    union_find(&mut parent, i, j);
                }
            }
        }

        let mut groups: HashMap<usize, Vec<usize>> = HashMap::new();
        for i in 0..n {
            let root = find(&mut parent, i);
            groups.entry(root).or_default().push(i);
        }

        // Keep only real groups (2+ pages) and number them deterministically by smallest URL.
        let mut real_groups: Vec<Vec<usize>> = groups.into_values().filter(|g| g.len() >= 2).collect();
        real_groups.sort_by(|a, b| {
            let min_url = |g: &[usize]| -> String { g.iter().map(|&i| pages[i].url.clone()).min().unwrap_or_default() };
            min_url(a.as_slice()).cmp(&min_url(b.as_slice()))
        });

        let mut group_of_page: HashMap<usize, (usize, usize)> = HashMap::new(); // page idx -> (group no, group size)
        for (gi, group) in real_groups.iter().enumerate() {
            for &i in group {
                group_of_page.insert(i, (gi + 1, group.len()));
            }
        }

        // ---- Per-page status -----------------------------------------------------------------
        #[derive(PartialEq, Eq, Clone)]
        enum PageStatus {
            ExactDup,
            NearDup,
            Thin,
            Unique,
        }

        let mut page_status: Vec<PageStatus> = Vec::new();
        for (i, p) in pages.iter().enumerate() {
            let status = match group_of_page.get(&i) {
                Some((gi, _)) => {
                    // Exact duplicate = another page in the group has the identical fingerprint.
                    let exact = real_groups[gi - 1]
                        .iter()
                        .any(|&k| k != i && pages[k].fingerprint == p.fingerprint);
                    if exact {
                        PageStatus::ExactDup
                    } else {
                        PageStatus::NearDup
                    }
                }
                None => {
                    if p.words < THIN_WORD_THRESHOLD {
                        PageStatus::Thin
                    } else {
                        PageStatus::Unique
                    }
                }
            };
            page_status.push(status);
        }

        // Count distinct variant groups (groups made only of URL variants of the same path).
        let variant_groups: usize = real_groups
            .iter()
            .filter(|g| {
                let urls: Vec<String> = g.iter().map(|&k| pages[k].url.clone()).collect();
                group_is_url_variant(&urls)
            })
            .count();

        // ---- Table ---------------------------------------------------------------------------
        let console_width = utils::get_console_width();
        let page_width = (console_width as i32 - 110).clamp(30, 200);

        let columns = vec![
            SuperTableColumn::new(
                "page".to_string(),
                "Page".to_string(),
                page_width,
                None,
                None,
                true,
                false,
                false,
                true,
                None,
            ),
            SuperTableColumn::new(
                "words".to_string(),
                "Visible words".to_string(),
                14,
                None,
                None,
                false,
                false,
                false,
                true,
                None,
            ),
            SuperTableColumn::new(
                "group".to_string(),
                "Dup. group".to_string(),
                11,
                None,
                None,
                false,
                false,
                false,
                true,
                None,
            ),
            SuperTableColumn::new(
                "groupPages".to_string(),
                "Group size".to_string(),
                11,
                None,
                None,
                false,
                false,
                false,
                true,
                None,
            ),
            SuperTableColumn::new(
                "similarity".to_string(),
                "Similarity".to_string(),
                12,
                None,
                None,
                false,
                false,
                false,
                true,
                None,
            ),
            SuperTableColumn::new(
                "status".to_string(),
                "Status".to_string(),
                15,
                None,
                None,
                false,
                false,
                false,
                true,
                None,
            ),
            SuperTableColumn::new(
                "recommendation".to_string(),
                "Recommendation".to_string(),
                60,
                None,
                None,
                false,
                false,
                false,
                true,
                None,
            ),
        ];

        // Approximate similarity from Hamming distance to the nearest other page in the group.
        fn similarity_of(pages: &[PageFingerprint], i: usize, group: &[usize]) -> Option<String> {
            let fi = pages[i].fingerprint?;
            let mut best: Option<u32> = None;
            for &k in group {
                if k == i {
                    continue;
                }
                let fj = pages[k].fingerprint?;
                let d = hamming_distance(fi, fj);
                if best.map(|b| d < b).unwrap_or(true) {
                    best = Some(d);
                }
            }
            best.map(|d| format!("~{}%", 100 - (d as u64 * 100 / 64)))
        }

        let mut rows: Vec<(usize, PageStatus)> = pages
            .iter()
            .enumerate()
            .map(|(i, _)| (i, page_status[i].clone()))
            .collect();
        let status_rank = |st: &PageStatus| match st {
            PageStatus::ExactDup => 0,
            PageStatus::NearDup => 1,
            PageStatus::Thin => 2,
            PageStatus::Unique => 3,
        };
        rows.sort_by(|a, b| {
            status_rank(&a.1)
                .cmp(&status_rank(&b.1))
                .then_with(|| pages[a.0].url.cmp(&pages[b.0].url))
        });

        let mut data: Vec<HashMap<String, String>> = Vec::new();
        for (i, st) in rows {
            let p = &pages[i];
            let (group_no, group_size) = group_of_page.get(&i).copied().unwrap_or((0, 0));
            let group = if group_no > 0 {
                Some(real_groups[group_no - 1].clone())
            } else {
                None
            };

            let group_label = if group_no > 0 {
                format!("G{}", group_no)
            } else {
                "—".to_string()
            };
            let size_label = if group_no > 0 {
                group_size.to_string()
            } else {
                "—".to_string()
            };
            let similarity = group
                .as_ref()
                .and_then(|g| similarity_of(&pages, i, g))
                .unwrap_or_else(|| "—".to_string());

            let status_label = match st {
                PageStatus::ExactDup => "exact duplicate",
                PageStatus::NearDup => "near-duplicate",
                PageStatus::Thin => "thin content",
                PageStatus::Unique => "unique",
            };

            let recommendation = match st {
                PageStatus::ExactDup | PageStatus::NearDup => {
                    if let Some(g) = &group {
                        let rep = g
                            .iter()
                            .min_by_key(|&&k| (pages[k].url.len(), pages[k].url.clone()))
                            .map(|&k| &pages[k].url);
                        if let Some(rep_url) = rep {
                            format!(
                                "Keep '{}', 301-redirect or noindex the others (or point rel=canonical at it)",
                                rep_url
                            )
                        } else {
                            "Keep one page, 301-redirect or noindex the others (or add canonical)".to_string()
                        }
                    } else {
                        "Keep one page, 301-redirect or noindex the others (or add canonical)".to_string()
                    }
                }
                PageStatus::Thin => format!(
                    "Expand the content (currently {} visible words; under the {}-word threshold) or merge it into a related page",
                    p.words, THIN_WORD_THRESHOLD
                ),
                PageStatus::Unique => "—".to_string(),
            };

            let mut row = HashMap::new();
            row.insert("page".to_string(), p.path_query.clone());
            row.insert("words".to_string(), p.words.to_string());
            row.insert("group".to_string(), group_label);
            row.insert("groupPages".to_string(), size_label);
            row.insert("similarity".to_string(), similarity);
            row.insert("status".to_string(), status_label.to_string());
            row.insert("recommendation".to_string(), recommendation);
            data.push(row);
        }

        let mut table = SuperTable::new(
            SUPER_TABLE_DUPLICATES.to_string(),
            "Duplicate & thin content".to_string(),
            "No HTML pages to analyze.".to_string(),
            columns,
            true,
            Some("page".to_string()),
            "ASC".to_string(),
            Some(
                "Near-duplicate content (64-bit SimHash of each page's visible main text, grouped when within \
                 4 bits of Hamming distance) and thin content (fewer than 100 visible words). Groups list their \
                 representative (shortest URL) to keep; the rest should be 301-redirected, noindexed or canonicalized. \
                 Groups made only of URL variants (same path, different query) are the classic faceted-utm pagination problem."
                    .to_string(),
            ),
            None,
            None,
        );
        table.set_data(data);
        status.configure_super_table_url_stripping(&mut table);
        output.add_super_table(&table);
        status.add_super_table_at_beginning(table);

        // ---- Summary ---------------------------------------------------------------------------
        let thin_pages: usize = pages
            .iter()
            .enumerate()
            .filter(|(i, _)| matches!(page_status[*i], PageStatus::Thin))
            .count();
        let group_count = real_groups.len();
        let pages_affected: usize = real_groups.iter().map(|g| g.len()).sum();

        if group_count == 0 {
            status.add_ok_to_summary("content-duplicates", "No near-duplicate content groups detected.");
        } else if group_count >= 3 {
            status.add_critical_to_summary(
                "content-duplicates",
                &format!(
                    "{} near-duplicate content groups detected ({} page(s) affected)",
                    group_count, pages_affected
                ),
            );
        } else {
            status.add_warning_to_summary(
                "content-duplicates",
                &format!(
                    "{} near-duplicate content group(s) detected ({} page(s) affected)",
                    group_count, pages_affected
                ),
            );
        }

        if variant_groups > 0 {
            status.add_notice_to_summary(
                "content-variants",
                &format!(
                    "{} near-duplicate group(s) contain only URL-variant pages (same path, different query parameters) — the classic faceted/utm/pagination problem",
                    variant_groups
                ),
            );
        }

        if thin_pages == 0 {
            status.add_ok_to_summary("content-thin", "No thin-content pages detected.");
        } else if thin_pages >= 4 {
            status.add_critical_to_summary(
                "content-thin",
                &format!(
                    "{} thin-content pages detected (fewer than {} visible words each)",
                    thin_pages, THIN_WORD_THRESHOLD
                ),
            );
        } else {
            status.add_warning_to_summary(
                "content-thin",
                &format!(
                    "{} thin-content page(s) detected (fewer than {} visible words each)",
                    thin_pages, THIN_WORD_THRESHOLD
                ),
            );
        }

        if group_count == 0 && thin_pages == 0 {
            status.add_ok_to_summary(
                "duplicate-content",
                &format!(
                    "All {} indexable page(s) have unique content of sufficient length ({}+ visible words).",
                    pages.len(),
                    THIN_WORD_THRESHOLD
                ),
            );
        }

        self.base.measure_exec_time("DuplicateContentAnalyzer", "analyze", s);
    }

    fn should_be_activated(&self) -> bool {
        true
    }

    fn get_order(&self) -> i32 {
        116
    }

    fn get_name(&self) -> &str {
        "DuplicateContentAnalyzer"
    }

    fn get_exec_times(&self) -> &HashMap<String, f64> {
        self.base.get_exec_times()
    }

    fn get_exec_counts(&self) -> &HashMap<String, usize> {
        self.base.get_exec_counts()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fnv1a_is_stable() {
        assert_eq!(fnv1a_64(b"hello"), fnv1a_64(b"hello"));
        assert_ne!(fnv1a_64(b"hello"), fnv1a_64(b"world"));
    }

    #[test]
    fn hamming_identical_and_opposite() {
        assert_eq!(hamming_distance(0u64, 0u64), 0);
        assert_eq!(hamming_distance(0u64, u64::MAX), 64);
        assert_eq!(hamming_distance(0b1011u64, 0b1001u64), 1);
    }

    #[test]
    fn short_text_no_fingerprint() {
        assert!(simhash_fingerprint("too few words").is_none());
    }

    #[test]
    fn identical_text_same_fingerprint() {
        let a = "the quick brown fox jumps over the lazy dog near the river every single morning without fail";
        let b = "the quick brown fox jumps over the lazy dog near the river every single morning without fail";
        assert_eq!(simhash_fingerprint(a), simhash_fingerprint(b));
    }

    #[test]
    fn very_different_text_far_apart() {
        let a = "alpha beta gamma delta epsilon zeta eta theta iota kappa lambda mu nu xi omicron pi rho";
        let b = "one two three four five six seven eight nine ten eleven twelve thirteen fourteen fifteen sixteen";
        let (fa, fb) = (simhash_fingerprint(a).unwrap(), simhash_fingerprint(b).unwrap());
        assert!(hamming_distance(fa, fb) > NEAR_DUP_HAMMING);
    }

    // Deterministic long, diverse test text (~600 distinct words) — representative of a real
    // content page. Distinct tokens avoid the bit-margin noise that short/repetitive text has.
    fn long_diverse_page(n: usize) -> String {
        (0..n).map(|i| format!("tok{:05}", i)).collect::<Vec<_>>().join(" ")
    }

    #[test]
    fn near_identical_text_close() {
        // A near-duplicate (a couple of words changed on a content-length page) stays within
        // the near-dup threshold.
        let base = long_diverse_page(600);
        let variant = base.replace("tok00040", "zzz0040").replace("tok00120", "zzz0120");
        let fa = simhash_fingerprint(&base).unwrap();
        let fb = simhash_fingerprint(&variant).unwrap();
        let d = hamming_distance(fa, fb);
        assert!(
            d <= NEAR_DUP_HAMMING,
            "2-word change on a 600-word page should be a near-dup, got distance={d}"
        );
    }

    #[test]
    fn unrelated_text_far_apart_at_page_length() {
        let base = long_diverse_page(600);
        // A different token vocabulary (offset by 10000) = genuinely unrelated page.
        let unrelated = long_diverse_page(600).replace("tok", "tokx");
        let fa = simhash_fingerprint(&base).unwrap();
        let fb = simhash_fingerprint(&unrelated).unwrap();
        assert!(
            hamming_distance(fa, fb) > NEAR_DUP_HAMMING,
            "unrelated pages must not be flagged near-duplicate, got distance={}",
            hamming_distance(fa, fb)
        );
    }

    #[test]
    fn variant_group_detection() {
        let urls = vec![
            "https://ex.com/products?color=red".to_string(),
            "https://ex.com/products?color=blue".to_string(),
        ];
        assert!(group_is_url_variant(&urls));
        let urls2 = vec![
            "https://ex.com/products/red".to_string(),
            "https://ex.com/products/blue".to_string(),
        ];
        assert!(!group_is_url_variant(&urls2));
    }
}
