// SiteOne Crawler - Visible text extraction for analyzers
// (c) Jan Reges <jan.reges@siteone.cz>
//
// Extracts the page's visible main content text (the page's own content, excluding site chrome
// such as header/footer/nav/aside, hidden and collapsed content, scripts, styles, SVG, etc.).
// Reuses the AI pipeline's block splitter so analyzers and AI reports see the same "visible text".

use crate::ai::blocks::{Region, blocks_from_html};

/// Visible text of the page's own content, blocks in document order, joined by single spaces.
pub fn visible_main_text(html: &str) -> String {
    blocks_from_html(html)
        .iter()
        .filter(|b| b.region == Region::Main && !b.collapsed && !b.hidden)
        .map(|b| b.text.as_str())
        .collect::<Vec<&str>>()
        .join(" ")
}

/// Number of words in the page's visible main content text.
pub fn visible_main_word_count(html: &str) -> usize {
    visible_main_text(html).split_whitespace().count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn excludes_site_chrome_and_hidden() {
        let html = r#"
        <html><body>
            <header>Nav bar Home About Contact</header>
            <main><p>Visible main content words here</p></main>
            <div style="display:none">Hidden secret words</div>
            <details><summary>More</summary><p>Collapsed body words</p></details>
            <footer>Footer copyright text</footer>
        </body></html>"#;
        let text = visible_main_text(html);
        assert!(text.contains("Visible main content words here"));
        assert!(!text.contains("Nav bar"));
        assert!(!text.contains("Footer copyright"));
        assert!(!text.contains("Hidden secret words"));
    }

    #[test]
    fn word_count_basic() {
        let html = "<html><body><main><p>one two three</p></main></body></html>";
        assert_eq!(visible_main_word_count(html), 3);
    }
}
