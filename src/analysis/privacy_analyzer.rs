// SiteOne Crawler - PrivacyAnalyzer
// (c) Jan Reges <jan.reges@siteone.cz>
//
// Surfaces privacy-relevant signals on a page: whether a cookie-consent UI is present, whether
// cookies are set (Set-Cookie response header or document.cookie), and whether the page uses
// localStorage/sessionStorage. Useful for GDPR/cookie-compliance review: a site that sets
// cookies or stores local data without any consent UI is a common compliance gap.

use std::collections::HashMap;
use std::time::Instant;

use regex::Regex;

use crate::analysis::analyzer::Analyzer;
use crate::analysis::base_analyzer::BaseAnalyzer;
use crate::analysis::result::analyzer_stats::AnalyzerStats;
use crate::analysis::result::url_analysis_result::UrlAnalysisResult;
use crate::components::super_table::SuperTable;
use crate::components::super_table_column::SuperTableColumn;
use crate::extra_column::ExtraColumn;
use crate::output::output::Output;
use crate::result::status::Status;
use crate::result::visited_url::VisitedUrl;
use crate::types::ContentTypeId;
use crate::utils;

const ANALYSIS_CONSENT: &str = "Consent";
const ANALYSIS_COOKIES: &str = "Cookies set";
const ANALYSIS_STORAGE: &str = "Local storage";

pub struct PrivacyAnalyzer {
    base: BaseAnalyzer,
    stats: AnalyzerStats,
    pages_with_consent: usize,
    pages_without_consent: usize,
    pages_setting_cookies: usize,
    pages_using_storage: usize,
    html_pages: usize,
}

impl PrivacyAnalyzer {
    pub fn new() -> Self {
        Self {
            base: BaseAnalyzer::new(),
            stats: AnalyzerStats::new(),
            pages_with_consent: 0,
            pages_without_consent: 0,
            pages_setting_cookies: 0,
            pages_using_storage: 0,
            html_pages: 0,
        }
    }

    fn detect_consent_ui(html: &str) -> bool {
        // Any element id/class referencing a cookie/consent/GDPR banner or modal.
        let re = Regex::new(r#"(?i)(?:id|class)\s*=\s*["'][^"']*(?:cookie|consent|gdpr)"#).unwrap();
        re.is_match(html)
    }

    fn detect_local_storage(html: &str) -> bool {
        let re = Regex::new(r#"(?i)\b(localStorage|sessionStorage)\b"#).unwrap();
        re.is_match(html)
    }

    fn document_uses_cookie(html: &str) -> bool {
        let re = Regex::new(r"(?i)document\.cookie").unwrap();
        re.is_match(html)
    }

    fn has_set_cookie_header(headers: Option<&HashMap<String, String>>) -> bool {
        match headers {
            Some(h) => h.keys().any(|k| k.eq_ignore_ascii_case("set-cookie")),
            None => false,
        }
    }

    fn set_findings_to_summary(&self, status: &Status) {
        if self.html_pages == 0 {
            return;
        }
        if self.pages_setting_cookies > 0 && self.pages_with_consent == 0 {
            status.add_warning_to_summary(
                "privacy-consent",
                &format!(
                    "{} page(s) set cookies but no cookie-consent UI was detected anywhere",
                    self.pages_setting_cookies
                ),
            );
        } else if self.pages_setting_cookies > 0 && self.pages_without_consent > 0 {
            status.add_notice_to_summary(
                "privacy-consent",
                &format!(
                    "Cookies are set on {} page(s); consent UI found on {} page(s) but missing on {}",
                    self.pages_setting_cookies, self.pages_with_consent, self.pages_without_consent
                ),
            );
        } else if self.pages_with_consent > 0 {
            status.add_ok_to_summary(
                "privacy-consent",
                &format!("Cookie-consent UI detected on {} page(s)", self.pages_with_consent),
            );
        } else {
            status.add_ok_to_summary("privacy-consent", "No cookies detected and no consent UI required");
        }

        if self.pages_using_storage > 0 {
            status.add_info_to_summary(
                "privacy-storage",
                &format!("{} page(s) use localStorage/sessionStorage", self.pages_using_storage),
            );
        }
    }
}

impl Default for PrivacyAnalyzer {
    fn default() -> Self {
        Self::new()
    }
}

impl Analyzer for PrivacyAnalyzer {
    fn analyze(&mut self, status: &Status, output: &mut dyn Output) {
        let s = Instant::now();

        let columns = vec![
            SuperTableColumn::new(
                "analysisName".to_string(),
                "Analysis".to_string(),
                -1,
                None,
                None,
                true,
                false,
                false,
                true,
                None,
            ),
            SuperTableColumn::new(
                "ok".to_string(),
                "OK".to_string(),
                5,
                Some(Box::new(|value: &str, _r: &str| {
                    if let Ok(v) = value.parse::<usize>()
                        && v > 0
                    {
                        return utils::get_color_text(&v.to_string(), "green", false);
                    }
                    "0".to_string()
                })),
                None,
                false,
                false,
                false,
                true,
                None,
            ),
            SuperTableColumn::new(
                "notice".to_string(),
                "Notice".to_string(),
                6,
                Some(Box::new(|value: &str, _r: &str| {
                    if let Ok(v) = value.parse::<usize>()
                        && v > 0
                    {
                        return utils::get_color_text(&v.to_string(), "blue", false);
                    }
                    "0".to_string()
                })),
                None,
                false,
                false,
                false,
                true,
                None,
            ),
            SuperTableColumn::new(
                "warning".to_string(),
                "Warning".to_string(),
                7,
                Some(Box::new(|value: &str, _r: &str| {
                    if let Ok(v) = value.parse::<usize>()
                        && v > 0
                    {
                        return utils::get_color_text(&v.to_string(), "magenta", true);
                    }
                    "0".to_string()
                })),
                None,
                false,
                false,
                false,
                true,
                None,
            ),
            SuperTableColumn::new(
                "critical".to_string(),
                "Critical".to_string(),
                8,
                Some(Box::new(|value: &str, _r: &str| {
                    if let Ok(v) = value.parse::<usize>()
                        && v > 0
                    {
                        return utils::get_color_text(&v.to_string(), "red", true);
                    }
                    "0".to_string()
                })),
                None,
                false,
                false,
                false,
                true,
                None,
            ),
        ];

        let data = self.stats.to_table_data();

        let mut super_table = SuperTable::new(
            "privacy".to_string(),
            "Privacy".to_string(),
            "Nothing to report.".to_string(),
            columns,
            true,
            None,
            "ASC".to_string(),
            Some(
                "Per-page privacy signals: consent UI, cookies set, and localStorage/sessionStorage usage.".to_string(),
            ),
            None,
            None,
        );
        super_table.set_data(data);
        status.configure_super_table_url_stripping(&mut super_table);
        output.add_super_table(&super_table);
        status.add_super_table_at_end(super_table);

        self.set_findings_to_summary(status);
        self.base.measure_exec_time("PrivacyAnalyzer", "analyze", s);
    }

    fn analyze_visited_url(
        &mut self,
        visited_url: &VisitedUrl,
        body: Option<&str>,
        headers: Option<&HashMap<String, String>>,
    ) -> Option<UrlAnalysisResult> {
        let is_html =
            visited_url.content_type == ContentTypeId::Html && visited_url.status_code == 200 && body.is_some();
        if !is_html {
            return None;
        }
        let html = body?;
        let mut result = UrlAnalysisResult::new();
        self.html_pages += 1;

        let has_consent = Self::detect_consent_ui(html);
        let sets_cookies = Self::has_set_cookie_header(headers) || Self::document_uses_cookie(html);
        let uses_storage = Self::detect_local_storage(html);

        if has_consent {
            result.add_ok("Cookie-consent UI present".to_string(), ANALYSIS_CONSENT, None);
            self.stats.add_ok(ANALYSIS_CONSENT, Some(&visited_url.url));
            self.pages_with_consent += 1;
        } else {
            result.add_notice(
                "No cookie-consent UI detected".to_string(),
                ANALYSIS_CONSENT,
                Some(vec![
                    "No element id/class matching cookie/consent/gdpr was found on this page.".to_string(),
                ]),
            );
            self.stats.add_notice(ANALYSIS_CONSENT, Some(&visited_url.url));
            self.pages_without_consent += 1;
        }

        if sets_cookies {
            let source = if Self::has_set_cookie_header(headers) {
                "Set-Cookie response header".to_string()
            } else {
                "document.cookie in scripts".to_string()
            };
            result.add_notice(
                format!("Cookies are set ({})", source),
                ANALYSIS_COOKIES,
                Some(vec![source]),
            );
            self.stats.add_notice(ANALYSIS_COOKIES, Some(&visited_url.url));
            self.pages_setting_cookies += 1;
        }

        if uses_storage {
            result.add_notice(
                "Page uses localStorage/sessionStorage".to_string(),
                ANALYSIS_STORAGE,
                Some(vec![
                    "Local data is stored in the browser (check consent + data-protection disclosures).".to_string(),
                ]),
            );
            self.stats.add_notice(ANALYSIS_STORAGE, Some(&visited_url.url));
            self.pages_using_storage += 1;
        }

        Some(result)
    }

    fn show_analyzed_visited_url_result_as_column(&self) -> Option<ExtraColumn> {
        ExtraColumn::new("Privacy".to_string(), Some(8), false, None, None, None).ok()
    }

    fn should_be_activated(&self) -> bool {
        true
    }

    fn get_order(&self) -> i32 {
        127
    }

    fn get_name(&self) -> &str {
        "PrivacyAnalyzer"
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
    fn detects_consent_ui() {
        assert!(PrivacyAnalyzer::detect_consent_ui(
            r#"<div id="cookie-banner" class="cookie-consent"><button>Accept all</button></div>"#
        ));
        assert!(!PrivacyAnalyzer::detect_consent_ui(
            "<html><body><p>hello</p></body></html>"
        ));
    }

    #[test]
    fn detects_local_storage() {
        assert!(PrivacyAnalyzer::detect_local_storage(
            "<script>localStorage.setItem('theme','dark');</script>"
        ));
        assert!(!PrivacyAnalyzer::detect_local_storage("<p>no storage here</p>"));
    }

    #[test]
    fn detects_document_cookie() {
        assert!(PrivacyAnalyzer::document_uses_cookie(
            "<script>document.cookie = 'a=b; path=/';</script>"
        ));
        assert!(!PrivacyAnalyzer::document_uses_cookie("<p>cookies are tasty</p>"));
    }

    #[test]
    fn set_cookie_header_case_insensitive() {
        let mut h = HashMap::new();
        h.insert("Set-Cookie".to_string(), "sid=1".to_string());
        assert!(PrivacyAnalyzer::has_set_cookie_header(Some(&h)));
        assert!(!PrivacyAnalyzer::has_set_cookie_header(None));
    }
}
