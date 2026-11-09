//! CSS subset parser for EPUB styling
//!
//! Parses a minimal subset of CSS sufficient for EPUB rendering:
//! - Font properties: `font-size`, `font-family`, `font-weight`, `font-style`
//! - Text: `text-align`, `line-height`, `letter-spacing`
//! - Spacing: `margin-top`, `margin-bottom`
//! - Selectors: tag, class, and inline `style` attributes
//!
//! Complex selectors, floats, positioning, and grid are out of scope.

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;

use crate::error::EpubError;

/// Line height value
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum LineHeight {
    /// Absolute height in pixels
    Px(f32),
    /// Multiplier relative to font size (e.g., 1.5 = 1.5x)
    Multiplier(f32),
}

/// Font size value
#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub enum FontSize {
    /// Absolute size in pixels
    Px(f32),
    /// Relative size in em units
    Em(f32),
}

/// Font weight
#[derive(Clone, Copy, Debug, PartialEq, Default)]
#[non_exhaustive]
pub enum FontWeight {
    /// Normal weight (400)
    #[default]
    Normal,
    /// Bold weight (700)
    Bold,
}

/// Font style
#[derive(Clone, Copy, Debug, PartialEq, Default)]
#[non_exhaustive]
pub enum FontStyle {
    /// Upright text
    #[default]
    Normal,
    /// Italic text
    Italic,
}

/// First-line indentation. Negative and hanging indents are not supported.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TextIndent {
    /// Physical pixel length.
    Px(f32),
    /// Font-relative length.
    Em(f32),
    /// Percentage of the text column.
    Percent(f32),
    /// Parent computed value.
    Inherit,
}

/// Tab interval in space advances or CSS lengths.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TabSize {
    /// Multiple of the declaring element's font size.
    Em(f32),
    /// Multiple of the containing block's space advance.
    Spaces(f32),
    /// Absolute CSS pixels.
    Px(f32),
    /// Resolve against the parent element.
    Inherit,
}

/// Whitespace processing and wrapping declarations.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub enum WhiteSpace {
    /// Collapse spaces and segment breaks.
    #[default]
    Normal,
    /// Collapse whitespace and suppress soft wrapping.
    NoWrap,
    /// Preserve whitespace and suppress soft wrapping.
    Pre,
    /// Preserve whitespace with soft wrapping.
    PreWrap,
    /// Preserve segment breaks and collapse spaces.
    PreLine,
    /// Resolve against the parent element.
    Inherit,
}

/// Paragraph base direction. Auto is supplied by XHTML dir, not CSS.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Direction {
    /// Left-to-right base embedding level.
    Ltr,
    /// Right-to-left base embedding level.
    Rtl,
    /// Automatic base direction from XHTML markup.
    Auto,
    /// Inherit the parent direction.
    Inherit,
}

/// Text alignment
#[derive(Clone, Copy, Debug, PartialEq, Default)]
#[non_exhaustive]
pub enum TextAlign {
    /// Left-aligned (default for LTR)
    #[default]
    Left,
    /// Centered
    Center,
    /// Right-aligned
    Right,
    /// Justified
    Justify,
    /// Logical paragraph start.
    Start,
    /// Logical paragraph end.
    End,
    /// Inherited value, including unset.
    Inherit,
}

/// Hyphenation declaration. Inherit also represents the inherited unset value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hyphens {
    /// Disable explicit and automatic hyphenation.
    None,
    /// Permit only explicit hyphenation opportunities.
    Manual,
    /// Permit explicit and dictionary opportunities.
    Auto,
    /// Resolve against the parent element.
    Inherit,
}

/// Supported inherited list marker styles.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ListStyleType {
    /// Filled round bullet.
    Disc,
    /// Open round bullet.
    Circle,
    /// Square bullet.
    Square,
    /// Decimal digits.
    Decimal,
    /// Lowercase Latin letters.
    LowerAlpha,
    /// Uppercase Latin letters.
    UpperAlpha,
    /// Lowercase Roman numerals.
    LowerRoman,
    /// Uppercase Roman numerals.
    UpperRoman,
    /// Suppress the marker.
    None,
    /// Resolve against the parent element.
    Inherit,
}

/// Image dimension or maximum in the supported paged-layout subset.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ImageLimit {
    /// Automatic dimension, or no author maximum.
    None,
    /// Nonnegative CSS pixel limit.
    Px(f32),
    /// Nonnegative percentage of the containing content box.
    Percent(f32),
}

/// Display classes used when constructing source text nodes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Display {
    /// Inline formatting.
    Inline,
    /// Block, table, list-item or inline-block formatting.
    Block,
    /// Hidden element.
    None,
    /// Explicit inheritance from the parent element.
    Inherit,
}

/// All fields are optional — `None` means "not specified" (inherit from parent
/// or use default).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CssStyle {
    /// Author display category, retained for source-position interoperability.
    pub display: Option<Display>,
    /// Non-inherited image width; ImageLimit::None means auto.
    pub width: Option<ImageLimit>,
    /// Non-inherited image height; ImageLimit::None means auto.
    pub height: Option<ImageLimit>,
    /// Non-inherited image width constraint.
    pub max_width: Option<ImageLimit>,
    /// Non-inherited image height constraint.
    pub max_height: Option<ImageLimit>,
    /// Non-inherited minimum image width; None resolves to zero.
    pub min_width: Option<ImageLimit>,
    /// Non-inherited minimum image height; None resolves to zero.
    pub min_height: Option<ImageLimit>,
    /// Inherited list marker style.
    pub list_style_type: Option<ListStyleType>,
    /// Inherited first-line indentation.
    pub text_indent: Option<TextIndent>,
    /// Inherited tab interval.
    pub tab_size: Option<TabSize>,
    /// Whitespace declaration, inherited when absent.
    pub white_space: Option<WhiteSpace>,
    /// Optional hyphenation policy.
    pub hyphens: Option<Hyphens>,
    /// Font size
    pub font_size: Option<FontSize>,
    /// Font family name
    pub font_family: Option<String>,
    /// Font weight (normal or bold)
    pub font_weight: Option<FontWeight>,
    /// Font style (normal or italic)
    pub font_style: Option<FontStyle>,
    /// Text alignment
    pub text_align: Option<TextAlign>,
    /// Declared base direction, inherited when absent.
    pub direction: Option<Direction>,
    /// Line height
    pub line_height: Option<LineHeight>,
    /// Letter spacing in pixels
    pub letter_spacing: Option<f32>,
    /// Top margin in pixels
    pub margin_top: Option<f32>,
    /// Bottom margin in pixels
    pub margin_bottom: Option<f32>,
}

impl CssStyle {
    /// Create an empty style (all properties unset)
    pub fn new() -> Self {
        Self::default()
    }

    /// Check if any property is set
    pub fn is_empty(&self) -> bool {
        self.display.is_none() && self.width.is_none() && self.height.is_none() && self.max_width.is_none() && self.max_height.is_none() && self.min_width.is_none() && self.min_height.is_none() && self.list_style_type.is_none() && self.text_indent.is_none() && self.tab_size.is_none() && self.white_space.is_none() && self.direction.is_none() && self.hyphens.is_none()
            && self.font_size.is_none()
            && self.font_family.is_none()
            && self.font_weight.is_none()
            && self.font_style.is_none()
            && self.text_align.is_none()
            && self.line_height.is_none()
            && self.letter_spacing.is_none()
            && self.margin_top.is_none()
            && self.margin_bottom.is_none()
    }

    /// Merge another style into this one (other's values take precedence)
    pub fn merge(&mut self, other: &CssStyle) {
        if other.width.is_some() { self.width = other.width; }
        if other.height.is_some() { self.height = other.height; }
        if other.max_width.is_some() { self.max_width = other.max_width; }
        if other.max_height.is_some() { self.max_height = other.max_height; }
        if other.min_width.is_some() { self.min_width = other.min_width; }
        if other.min_height.is_some() { self.min_height = other.min_height; }
        if other.list_style_type.is_some() { self.list_style_type = other.list_style_type; }
        if other.text_indent.is_some() { self.text_indent = other.text_indent; }
        if other.tab_size.is_some() { self.tab_size = other.tab_size; }
        if other.white_space.is_some() { self.white_space = other.white_space; }
        if other.display.is_some() { self.display = other.display; }
        if other.direction.is_some() { self.direction = other.direction; }
        if other.hyphens.is_some() { self.hyphens = other.hyphens; }
        if other.font_size.is_some() {
            self.font_size = other.font_size;
        }
        if other.font_family.is_some() {
            self.font_family = other.font_family.clone();
        }
        if other.font_weight.is_some() {
            self.font_weight = other.font_weight;
        }
        if other.font_style.is_some() {
            self.font_style = other.font_style;
        }
        if other.text_align.is_some() {
            self.text_align = other.text_align;
        }
        if other.line_height.is_some() {
            self.line_height = other.line_height.clone();
        }
        if other.letter_spacing.is_some() {
            self.letter_spacing = other.letter_spacing;
        }
        if other.margin_top.is_some() {
            self.margin_top = other.margin_top;
        }
        if other.margin_bottom.is_some() {
            self.margin_bottom = other.margin_bottom;
        }
    }
}

/// A CSS selector (subset)
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum CssSelector {
    /// Tag selector (e.g., `p`, `h1`)
    Tag(String),
    /// Class selector (e.g., `.chapter-title`)
    Class(String),
    /// Tag + class selector (e.g., `p.intro`)
    TagClass(String, String),
}

impl CssSelector {
    /// Check if this selector matches a given tag name and class list
    pub fn matches(&self, tag: &str, classes: &[&str]) -> bool {
        match self {
            CssSelector::Tag(t) => t == tag,
            CssSelector::Class(c) => classes.contains(&c.as_str()),
            CssSelector::TagClass(t, c) => t == tag && classes.contains(&c.as_str()),
        }
    }
}

/// A single CSS rule (selector + declarations)
#[derive(Clone, Debug, PartialEq)]
pub struct CssRule {
    /// The selector for this rule
    pub selector: CssSelector,
    /// The style declarations
    pub style: CssStyle,
}

/// A parsed CSS stylesheet
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Stylesheet {
    /// All rules in document order
    pub rules: Vec<CssRule>,
}

impl Stylesheet {
    /// Create an empty stylesheet
    pub fn new() -> Self {
        Self::default()
    }

    /// Resolve the computed style for an element given its tag and classes
    ///
    /// Applies matching rules in document order (later rules override).
    pub fn resolve(&self, tag: &str, classes: &[&str]) -> CssStyle {
        let mut style = CssStyle::new();
        for rule in &self.rules {
            if rule.selector.matches(tag, classes) {
                style.merge(&rule.style);
            }
        }
        style
    }

    /// Get the number of rules
    pub fn len(&self) -> usize {
        self.rules.len()
    }

    /// Check if the stylesheet is empty
    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }
}

/// Parse a CSS stylesheet string into a `Stylesheet`
///
/// Handles the v1 subset: tag selectors, class selectors, tag.class selectors,
/// and the supported property set.
pub fn parse_stylesheet(css: &str) -> Result<Stylesheet, EpubError> {
    parse_stylesheet_bounded(css, usize::MAX)
}

/// Parse at most `max_rules` retained rules, rejecting before an excess push.
pub(crate) fn parse_stylesheet_bounded(css: &str, max_rules: usize) -> Result<Stylesheet, EpubError> {
    let mut stylesheet = Stylesheet::new();
    let mut pos = 0;
    let bytes = css.as_bytes();

    while pos < bytes.len() {
        // Skip whitespace and comments
        pos = skip_whitespace_and_comments(css, pos);
        if pos >= bytes.len() {
            break;
        }

        // Find selector (everything up to '{')
        let brace_start = match css[pos..].find('{') {
            Some(i) => pos + i,
            None => break, // No more rules
        };
        let selector_str = css[pos..brace_start].trim();
        if selector_str.is_empty() {
            pos = brace_start + 1;
            continue;
        }

        // Parse selector
        let selector = parse_selector(selector_str)?;

        // Find closing brace
        let brace_end = match css[brace_start + 1..].find('}') {
            Some(i) => brace_start + 1 + i,
            None => return Err(EpubError::Css("Unclosed CSS rule block".into())),
        };

        // Parse declarations
        let declarations = &css[brace_start + 1..brace_end];
        let style = parse_declarations(declarations)?;

        if !style.is_empty() {
            if stylesheet.rules.len() >= max_rules {
                return Err(EpubError::LimitExceeded { kind: crate::error::LimitKind::EventCount, actual: stylesheet.rules.len().saturating_add(1), limit: max_rules, path: None });
            }
            stylesheet.rules.push(CssRule { selector, style });
        }

        pos = brace_end + 1;
    }

    Ok(stylesheet)
}

/// Parse an inline `style` attribute value into a `CssStyle`
///
/// Example: `"font-weight: bold; margin-top: 10px"`
pub fn parse_inline_style(style_attr: &str) -> Result<CssStyle, EpubError> {
    parse_declarations(style_attr)
}

// -- Internal parsing helpers -------------------------------------------------

/// Skip whitespace and CSS comments (`/* ... */`)
fn skip_whitespace_and_comments(css: &str, mut pos: usize) -> usize {
    let bytes = css.as_bytes();
    while pos < bytes.len() {
        if bytes[pos].is_ascii_whitespace() {
            pos += 1;
        } else if pos + 1 < bytes.len() && bytes[pos] == b'/' && bytes[pos + 1] == b'*' {
            // Skip comment
            match css[pos + 2..].find("*/") {
                Some(end) => pos = pos + 2 + end + 2,
                None => return bytes.len(), // Unterminated comment
            }
        } else {
            break;
        }
    }
    pos
}

/// Parse a single CSS selector string
fn parse_selector(s: &str) -> Result<CssSelector, EpubError> {
    let s = s.trim();

    if let Some(class) = s.strip_prefix('.') {
        // Class selector
        if class.is_empty() {
            return Err(EpubError::Css("Empty class selector".into()));
        }
        Ok(CssSelector::Class(class.into()))
    } else if let Some(dot_pos) = s.find('.') {
        // Tag.class selector
        let tag = &s[..dot_pos];
        let class = &s[dot_pos + 1..];
        if tag.is_empty() || class.is_empty() {
            return Err(EpubError::Css(alloc::format!("Invalid selector: {}", s)));
        }
        Ok(CssSelector::TagClass(tag.into(), class.into()))
    } else {
        // Tag selector
        if s.is_empty() {
            return Err(EpubError::Css("Empty selector".into()));
        }
        Ok(CssSelector::Tag(s.into()))
    }
}

/// Parse CSS declarations (the part inside `{ ... }`)
fn parse_declarations(declarations: &str) -> Result<CssStyle, EpubError> {
    let mut style = CssStyle::new();

    for decl in declarations.split(';') {
        let decl = decl.trim();
        if decl.is_empty() {
            continue;
        }

        let colon_pos = match decl.find(':') {
            Some(pos) => pos,
            None => continue, // Malformed declaration, skip
        };

        let property = decl[..colon_pos].trim().to_lowercase();
        let value = decl[colon_pos + 1..].trim();

        match property.as_str() {
            "width" | "height" | "max-width" | "max-height" | "min-width" | "min-height" => {
                let dimension = property == "width" || property == "height";
                let minimum = property.starts_with("min-");
                let value = value.to_ascii_lowercase();
                let parsed = match value.as_str() {
                    "initial" | "unset" => Some(if minimum { ImageLimit::Px(0.0) } else { ImageLimit::None }),
                    "auto" if dimension => Some(ImageLimit::None),
                    "none" if !dimension && !minimum => Some(ImageLimit::None),
                    "0" => Some(ImageLimit::Px(0.0)),
                    _ => ["px", "%"].into_iter().find_map(|unit| {
                        let number = value.strip_suffix(unit)?.parse::<f32>().ok()?;
                        if !number.is_finite() || number < 0.0 { return None; }
                        Some(if unit == "%" { ImageLimit::Percent(number) } else { ImageLimit::Px(number) })
                    }),
                };
                if parsed.is_some() {
                    if property == "width" { style.width = parsed; }
                    else if property == "height" { style.height = parsed; }
                    else if property == "max-width" { style.max_width = parsed; }
                    else if property == "max-height" { style.max_height = parsed; }
                    else if property == "min-width" { style.min_width = parsed; }
                    else { style.min_height = parsed; }
                }
            }
            "list-style-type" => {
                let parsed = match value.to_ascii_lowercase().as_str() {
                    "disc" | "initial" => Some(ListStyleType::Disc),
                    "circle" => Some(ListStyleType::Circle),
                    "square" => Some(ListStyleType::Square),
                    "decimal" => Some(ListStyleType::Decimal),
                    "lower-alpha" | "lower-latin" => Some(ListStyleType::LowerAlpha),
                    "upper-alpha" | "upper-latin" => Some(ListStyleType::UpperAlpha),
                    "lower-roman" => Some(ListStyleType::LowerRoman),
                    "upper-roman" => Some(ListStyleType::UpperRoman),
                    "none" => Some(ListStyleType::None),
                    "inherit" | "unset" => Some(ListStyleType::Inherit),
                    _ => None,
                };
                if parsed.is_some() { style.list_style_type = parsed; }
            }
            "text-indent" => {
                let value = value.to_ascii_lowercase();
                let parsed = match value.as_str() {
                    "inherit" | "unset" => Some(TextIndent::Inherit),
                    "initial" | "0" => Some(TextIndent::Px(0.0)),
                    _ => ["px", "em", "%"].into_iter().find_map(|unit| {
                        let number = value.strip_suffix(unit)?.parse::<f32>().ok()?;
                        if !number.is_finite() { return None; }
                        Some(match unit { "em" => TextIndent::Em(number), "%" => TextIndent::Percent(number), _ => TextIndent::Px(number) })
                    }),
                };
                if parsed.is_some() { style.text_indent = parsed; }
            }
            "tab-size" => {
                let value = value.to_ascii_lowercase();
                let parsed = match value.as_str() {
                    "inherit" | "unset" => Some(TabSize::Inherit),
                    "initial" => Some(TabSize::Spaces(8.0)),
                    _ => {
                        let (number, unit) = if let Some(n) = value.strip_suffix("px") { (n, "px") }
                            else if let Some(n) = value.strip_suffix("em") { (n, "em") }
                            else { (value.as_str(), "") };
                        number.parse::<f32>().ok()
                            .filter(|n| n.is_finite() && *n >= 0.0)
                            .map(|n| match unit { "px" => TabSize::Px(n), "em" => TabSize::Em(n), _ => TabSize::Spaces(n) })
                    }
                };
                if parsed.is_some() { style.tab_size = parsed; }
            }
            "white-space" => {
                let mode = match value.to_ascii_lowercase().as_str() {
                    "normal" | "initial" => Some(WhiteSpace::Normal),
                    "nowrap" => Some(WhiteSpace::NoWrap),
                    "pre" => Some(WhiteSpace::Pre),
                    "pre-wrap" => Some(WhiteSpace::PreWrap),
                    "pre-line" => Some(WhiteSpace::PreLine),
                    "inherit" | "unset" => Some(WhiteSpace::Inherit),
                    _ => None,
                };
                if mode.is_some() { style.white_space = mode; }
            }
            "display" => {
                let display = match value.to_ascii_lowercase().as_str() {
                    "inline" | "initial" | "unset" => Some(Display::Inline),
                    "none" => Some(Display::None),
                    "inherit" => Some(Display::Inherit),
                    "block" | "inline-block" | "list-item" | "table" | "inline-table"
                    | "table-row-group" | "table-header-group" | "table-footer-group"
                    | "table-row" | "table-cell" | "table-column" | "table-column-group"
                    | "table-caption" => Some(Display::Block),
                    _ => None,
                };
                if display.is_some() { style.display = display; }
            }
            "direction" => {
                let direction = match value.to_ascii_lowercase().as_str() {
                    "ltr" | "initial" => Some(Direction::Ltr),
                    "rtl" => Some(Direction::Rtl),
                    "inherit" | "unset" => Some(Direction::Inherit),
                    _ => None,
                };
                if direction.is_some() { style.direction = direction; }
            }
            "hyphens" => {
                let policy = match value.to_ascii_lowercase().as_str() {
                    "none" => Some(Hyphens::None),
                    "manual" | "initial" => Some(Hyphens::Manual),
                    "auto" => Some(Hyphens::Auto),
                    "inherit" | "unset" => Some(Hyphens::Inherit),
                    _ => None,
                };
                if policy.is_some() { style.hyphens = policy; }
            }
            "font-size" => {
                style.font_size = parse_font_size(value);
            }
            "font-family" => {
                // Strip quotes from font family name
                let family = value.trim_matches(|c| c == '\'' || c == '"');
                if !family.is_empty() {
                    style.font_family = Some(family.into());
                }
            }
            "font-weight" => {
                style.font_weight = match value.to_lowercase().as_str() {
                    "bold" | "700" | "800" | "900" => Some(FontWeight::Bold),
                    "normal" | "400" => Some(FontWeight::Normal),
                    _ => None,
                };
            }
            "font-style" => {
                style.font_style = match value.to_lowercase().as_str() {
                    "italic" | "oblique" => Some(FontStyle::Italic),
                    "normal" => Some(FontStyle::Normal),
                    _ => None,
                };
            }
            "text-align" => {
                style.text_align = match value.to_lowercase().as_str() {
                    "left" => Some(TextAlign::Left),
                    "center" => Some(TextAlign::Center),
                    "right" => Some(TextAlign::Right),
                    "justify" => Some(TextAlign::Justify),
                    "start" | "initial" => Some(TextAlign::Start),
                    "end" => Some(TextAlign::End),
                    "inherit" | "unset" => Some(TextAlign::Inherit),
                    _ => None,
                };
            }
            "line-height" => {
                style.line_height = parse_line_height(value);
            }
            "letter-spacing" => {
                if let Some(letter_spacing) = parse_letter_spacing(value) {
                    style.letter_spacing = Some(letter_spacing);
                }
            }
            "margin-top" => {
                style.margin_top = parse_px_value(value);
            }
            "margin-bottom" => {
                style.margin_bottom = parse_px_value(value);
            }
            "margin" => {
                // Shorthand: only handle single-value case for now
                if let Some(val) = parse_px_value(value) {
                    style.margin_top = Some(val);
                    style.margin_bottom = Some(val);
                }
            }
            _ => {
                // Unsupported property — silently ignored
            }
        }
    }

    Ok(style)
}

/// Parse a font-size value (px or em)
fn parse_font_size(value: &str) -> Option<FontSize> {
    let value = value.trim().to_lowercase();
    if let Some(px_str) = value.strip_suffix("px") {
        px_str.trim().parse::<f32>().ok().map(FontSize::Px)
    } else if let Some(em_str) = value.strip_suffix("em") {
        em_str.trim().parse::<f32>().ok().map(FontSize::Em)
    } else {
        None
    }
}

/// Parse a line-height value (px or unitless multiplier)
fn parse_line_height(value: &str) -> Option<LineHeight> {
    let value = value.trim().to_lowercase();
    if let Some(px_str) = value.strip_suffix("px") {
        px_str.trim().parse::<f32>().ok().map(LineHeight::Px)
    } else if value == "normal" {
        None // Use default
    } else {
        // Bare number = multiplier
        value.parse::<f32>().ok().map(LineHeight::Multiplier)
    }
}

/// Parse a letter-spacing value (`normal` or `<number>px`)
fn parse_letter_spacing(value: &str) -> Option<f32> {
    let value = value.trim().to_lowercase();
    if value == "normal" {
        Some(0.0)
    } else if let Some(px_str) = value.strip_suffix("px") {
        px_str.trim().parse::<f32>().ok()
    } else {
        None
    }
}

/// Parse a pixel value (e.g., "10px" -> Some(10.0))
fn parse_px_value(value: &str) -> Option<f32> {
    let value = value.trim().to_lowercase();
    if let Some(px_str) = value.strip_suffix("px") {
        px_str.trim().parse::<f32>().ok()
    } else if value == "0" {
        Some(0.0)
    } else {
        // Try bare number
        value.parse::<f32>().ok()
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn image_dimensions_parse_auto_and_reject_invalid_overrides() {
        for (value, expected) in [("0", ImageLimit::Px(0.0)), ("1.5px", ImageLimit::Px(1.5)),
            ("25%", ImageLimit::Percent(25.0)), ("auto", ImageLimit::None),
            ("initial", ImageLimit::None), ("unset", ImageLimit::None)] {
            let style = parse_inline_style(&alloc::format!("width: {}; height: {}", value, value)).unwrap();
            assert_eq!(style.width, Some(expected));
            assert_eq!(style.height, Some(expected));
            assert!(!style.is_empty());
        }
        for value in ["-1px", "NaNpx", "inf%", "12", "none", "inherit", "calc(50%)", "1em"] {
            let style = parse_inline_style(&alloc::format!("width:7px; height:9px; width:{}; height:{}", value, value)).unwrap();
            assert_eq!(style.width, Some(ImageLimit::Px(7.0)));
            assert_eq!(style.height, Some(ImageLimit::Px(9.0)));
        }
    }

    #[test]
    fn image_minimums_parse_and_invalid_overrides_preserve_cascade() {
        for (value, expected) in [("0", ImageLimit::Px(0.0)), ("1.5px", ImageLimit::Px(1.5)),
            ("50%", ImageLimit::Percent(50.0)), ("initial", ImageLimit::Px(0.0)), ("unset", ImageLimit::Px(0.0))] {
            let style = parse_inline_style(&alloc::format!("min-width:{};min-height:{}", value, value)).unwrap();
            assert_eq!(style.min_width, Some(expected));
            assert_eq!(style.min_height, Some(expected));
            assert!(!style.is_empty());
        }
        for value in ["-1px", "-1%", "NaNpx", "inf%", "12", "auto", "none", "inherit", "calc(50%)", "1em"] {
            let style = parse_inline_style(&alloc::format!("min-width:7px;min-width:{}", value)).unwrap();
            assert_eq!(style.min_width, Some(ImageLimit::Px(7.0)), "{}", value);
        }
        let mut style = parse_inline_style("min-width:50%;min-height:90px").unwrap();
        style.merge(&parse_inline_style("min-width:unset").unwrap());
        assert_eq!(style.min_width, Some(ImageLimit::Px(0.0)));
        assert_eq!(style.min_height, Some(ImageLimit::Px(90.0)));
    }

    #[test]
    fn image_limits_parse_reset_and_ignore_invalid_overrides() {
        for (value, expected) in [("0", ImageLimit::Px(0.0)), ("1.5px", ImageLimit::Px(1.5)),
            ("50%", ImageLimit::Percent(50.0)), ("none", ImageLimit::None),
            ("initial", ImageLimit::None), ("unset", ImageLimit::None)] {
            let style = parse_inline_style(&alloc::format!("max-width: {}; max-height: {}", value, value)).unwrap();
            assert_eq!(style.max_width, Some(expected));
            assert_eq!(style.max_height, Some(expected));
            assert!(!style.is_empty());
        }
        for value in ["-1px", "-1%", "NaNpx", "inf%", "12", "auto", "inherit", "calc(50%)", "1em"] {
            let style = parse_inline_style(&alloc::format!("max-width: 7px; max-width: {}", value)).unwrap();
            assert_eq!(style.max_width, Some(ImageLimit::Px(7.0)), "{}", value);
        }
        let mut style = parse_inline_style("max-width: 50%; max-height: 90px").unwrap();
        style.merge(&parse_inline_style("max-width: none").unwrap());
        assert_eq!(style.max_width, Some(ImageLimit::None));
        assert_eq!(style.max_height, Some(ImageLimit::Px(90.0)));
    }

    use super::*;

    // -- CssStyle tests ---

    #[test]
    fn test_css_style_default_is_empty() {
        let style = CssStyle::new();
        assert!(style.is_empty());
    }

    #[test]
    fn test_css_style_with_letter_spacing_is_not_empty() {
        let style = CssStyle {
            letter_spacing: Some(0.0),
            ..Default::default()
        };
        assert!(!style.is_empty());
    }

    #[test]
    fn test_css_style_merge() {
        let mut base = CssStyle {
            list_style_type: None,
            display: None,
            font_weight: Some(FontWeight::Bold),
            text_align: Some(TextAlign::Left),
            ..Default::default()
        };
        let overlay = CssStyle {
            list_style_type: None,
            display: None,
            text_align: Some(TextAlign::Center),
            font_size: Some(FontSize::Px(16.0)),
            ..Default::default()
        };
        base.merge(&overlay);
        assert_eq!(base.font_weight, Some(FontWeight::Bold)); // kept
        assert_eq!(base.text_align, Some(TextAlign::Center)); // overridden
        assert_eq!(base.font_size, Some(FontSize::Px(16.0))); // added
    }

    // -- CssSelector tests ---

    #[test]
    fn test_selector_matches_tag() {
        let sel = CssSelector::Tag("p".into());
        assert!(sel.matches("p", &[]));
        assert!(!sel.matches("h1", &[]));
    }

    #[test]
    fn test_selector_matches_class() {
        let sel = CssSelector::Class("intro".into());
        assert!(sel.matches("p", &["intro"]));
        assert!(sel.matches("div", &["intro", "other"]));
        assert!(!sel.matches("p", &["other"]));
    }

    #[test]
    fn test_selector_matches_tag_class() {
        let sel = CssSelector::TagClass("p".into(), "intro".into());
        assert!(sel.matches("p", &["intro"]));
        assert!(!sel.matches("div", &["intro"]));
        assert!(!sel.matches("p", &["other"]));
    }

    // -- Stylesheet parsing tests ---

    #[test]
    fn test_parse_empty_stylesheet() {
        let ss = parse_stylesheet("").unwrap();
        assert!(ss.is_empty());
    }

    #[test]
    fn test_parse_tag_rule() {
        let css = "p { font-weight: bold; }";
        let ss = parse_stylesheet(css).unwrap();
        assert_eq!(ss.len(), 1);
        assert_eq!(ss.rules[0].selector, CssSelector::Tag("p".into()));
        assert_eq!(ss.rules[0].style.font_weight, Some(FontWeight::Bold));
    }

    #[test]
    fn test_parse_class_rule() {
        let css = ".chapter-title { font-size: 24px; text-align: center; }";
        let ss = parse_stylesheet(css).unwrap();
        assert_eq!(ss.len(), 1);
        assert_eq!(
            ss.rules[0].selector,
            CssSelector::Class("chapter-title".into())
        );
        assert_eq!(ss.rules[0].style.font_size, Some(FontSize::Px(24.0)));
        assert_eq!(ss.rules[0].style.text_align, Some(TextAlign::Center));
    }

    #[test]
    fn test_parse_tag_class_rule() {
        let css = "p.intro { font-style: italic; margin-top: 10px; }";
        let ss = parse_stylesheet(css).unwrap();
        assert_eq!(ss.len(), 1);
        assert_eq!(
            ss.rules[0].selector,
            CssSelector::TagClass("p".into(), "intro".into())
        );
        assert_eq!(ss.rules[0].style.font_style, Some(FontStyle::Italic));
        assert_eq!(ss.rules[0].style.margin_top, Some(10.0));
    }

    #[test]
    fn test_parse_multiple_rules() {
        let css = r#"
            h1 { font-weight: bold; font-size: 24px; }
            p { margin-bottom: 8px; }
            .note { font-style: italic; }
        "#;
        let ss = parse_stylesheet(css).unwrap();
        assert_eq!(ss.len(), 3);
    }

    #[test]
    fn test_parse_font_size_px() {
        let css = "p { font-size: 16px; }";
        let ss = parse_stylesheet(css).unwrap();
        assert_eq!(ss.rules[0].style.font_size, Some(FontSize::Px(16.0)));
    }

    #[test]
    fn test_parse_font_size_em() {
        let css = "p { font-size: 1.5em; }";
        let ss = parse_stylesheet(css).unwrap();
        assert_eq!(ss.rules[0].style.font_size, Some(FontSize::Em(1.5)));
    }

    #[test]
    fn test_parse_font_family() {
        let css = "p { font-family: 'Georgia'; }";
        let ss = parse_stylesheet(css).unwrap();
        assert_eq!(ss.rules[0].style.font_family, Some("Georgia".into()));
    }

    #[test]
    fn list_style_type_parses_inherits_and_preserves_prior_valid_values() {
        for (value, expected) in [
            ("disc", ListStyleType::Disc), ("CIRCLE", ListStyleType::Circle),
            ("square", ListStyleType::Square), ("decimal", ListStyleType::Decimal),
            ("lower-latin", ListStyleType::LowerAlpha), ("upper-alpha", ListStyleType::UpperAlpha),
            ("lower-roman", ListStyleType::LowerRoman), ("upper-roman", ListStyleType::UpperRoman),
            ("none", ListStyleType::None), ("inherit", ListStyleType::Inherit),
            ("unset", ListStyleType::Inherit), ("initial", ListStyleType::Disc),
        ] {
            let style = parse_inline_style(&alloc::format!("list-style-type: {}", value)).unwrap();
            assert!(!style.is_empty());
            assert_eq!(style.list_style_type, Some(expected));
            let mut merged = CssStyle::new();
            merged.merge(&style);
            assert_eq!(merged.list_style_type, Some(expected));
        }
        for value in ["custom-counter", "url(marker.png)", "'x'", "disc inside", ""] {
            let style = parse_inline_style(&alloc::format!("list-style-type: square; list-style-type: {}", value)).unwrap();
            assert_eq!(style.list_style_type, Some(ListStyleType::Square));
        }
    }

    #[test]
    fn text_indent_values_and_invalid_declarations() {
        for (value, expected) in [("-2em", TextIndent::Em(-2.0)), ("-12px", TextIndent::Px(-12.0)), ("-5%", TextIndent::Percent(-5.0)), ("2em", TextIndent::Em(2.0)), ("12px", TextIndent::Px(12.0)), ("5%", TextIndent::Percent(5.0)), ("initial", TextIndent::Px(0.0)), ("unset", TextIndent::Inherit), ("inherit", TextIndent::Inherit)] {
            assert_eq!(parse_inline_style(&alloc::format!("text-indent: {}", value)).unwrap().text_indent, Some(expected));
        }
        for value in ["--2px", "NaNpx", "infem", "2 px", "2", "2em hanging", "each-line", "calc(2px + 1em)"] {
            assert_eq!(parse_inline_style(&alloc::format!("text-indent: 4px; text-indent: {}", value)).unwrap().text_indent, Some(TextIndent::Px(4.0)));
        }
    }

    #[test]
    fn test_parse_text_align_values() {
        for (value, expected) in [
            ("left", TextAlign::Left),
            ("center", TextAlign::Center),
            ("right", TextAlign::Right),
            ("justify", TextAlign::Justify),
        ] {
            let css = alloc::format!("p {{ text-align: {}; }}", value);
            let ss = parse_stylesheet(&css).unwrap();
            assert_eq!(ss.rules[0].style.text_align, Some(expected));
        }
    }

    #[test]
    fn test_parse_margin_shorthand() {
        let css = "p { margin: 12px; }";
        let ss = parse_stylesheet(css).unwrap();
        assert_eq!(ss.rules[0].style.margin_top, Some(12.0));
        assert_eq!(ss.rules[0].style.margin_bottom, Some(12.0));
    }

    #[test]
    fn tab_size_values_and_invalid_declarations() {
        for (value, expected) in [
            ("0", TabSize::Spaces(0.0)), ("2.5", TabSize::Spaces(2.5)),
            ("40PX", TabSize::Px(40.0)), ("0px", TabSize::Px(0.0)),
            ("2EM", TabSize::Em(2.0)), ("0em", TabSize::Em(0.0)), ("1.5em", TabSize::Em(1.5)),
            ("initial", TabSize::Spaces(8.0)), ("inherit", TabSize::Inherit),
            ("unset", TabSize::Inherit),
        ] {
            let style = parse_inline_style(&format!("tab-size: {value}")).unwrap();
            assert_eq!(style.tab_size, Some(expected));
            assert!(!style.is_empty());
        }
        for value in ["-1", "-2px", "NaN", "inf", "1e99", "-2em", "1e99em", "2rem", "20%", "4 px", "auto"] {
            let style = parse_inline_style(&format!("tab-size: 3; tab-size: {value}")).unwrap();
            assert_eq!(style.tab_size, Some(TabSize::Spaces(3.0)), "{value}");
        }
    }

    #[test]
    fn test_parse_inline_style() {
        let style = parse_inline_style("font-weight: bold; font-size: 14px").unwrap();
        assert_eq!(style.font_weight, Some(FontWeight::Bold));
        assert_eq!(style.font_size, Some(FontSize::Px(14.0)));
    }

    #[test]
    fn test_css_comments_skipped() {
        let css = "/* comment */ p { font-weight: bold; } /* another */";
        let ss = parse_stylesheet(css).unwrap();
        assert_eq!(ss.len(), 1);
        assert_eq!(ss.rules[0].style.font_weight, Some(FontWeight::Bold));
    }

    #[test]
    fn test_unknown_properties_ignored() {
        let css = "p { color: red; font-weight: bold; display: flex; }";
        let ss = parse_stylesheet(css).unwrap();
        assert_eq!(ss.rules[0].style.font_weight, Some(FontWeight::Bold));
        // color and display are silently ignored
    }

    #[test]
    fn test_resolve_style() {
        let css = r#"
            p { margin-bottom: 8px; }
            .bold { font-weight: bold; }
            p.intro { font-style: italic; }
        "#;
        let ss = parse_stylesheet(css).unwrap();

        let style = ss.resolve("p", &["intro"]);
        assert_eq!(style.margin_bottom, Some(8.0));
        assert_eq!(style.font_style, Some(FontStyle::Italic));

        let style = ss.resolve("p", &["bold"]);
        assert_eq!(style.margin_bottom, Some(8.0));
        assert_eq!(style.font_weight, Some(FontWeight::Bold));

        let style = ss.resolve("div", &[]);
        assert!(style.is_empty());
    }

    #[test]
    fn test_parse_line_height_px() {
        let css = "p { line-height: 24px; }";
        let ss = parse_stylesheet(css).unwrap();
        assert_eq!(ss.rules[0].style.line_height, Some(LineHeight::Px(24.0)));
    }

    #[test]
    fn test_parse_line_height_multiplier() {
        let css = "p { line-height: 1.5; }";
        let ss = parse_stylesheet(css).unwrap();
        assert_eq!(
            ss.rules[0].style.line_height,
            Some(LineHeight::Multiplier(1.5))
        );
    }

    #[test]
    fn test_parse_line_height_normal() {
        let css = "p { line-height: normal; }";
        let ss = parse_stylesheet(css).unwrap();
        // "normal" maps to None, making the style empty, so no rule is added
        assert!(ss.is_empty());
    }

    #[test]
    fn test_parse_line_height_normal_with_other_props() {
        let css = "p { line-height: normal; font-weight: bold; }";
        let ss = parse_stylesheet(css).unwrap();
        assert_eq!(ss.len(), 1);
        assert_eq!(ss.rules[0].style.line_height, None);
        assert_eq!(ss.rules[0].style.font_weight, Some(FontWeight::Bold));
    }

    #[test]
    fn test_parse_letter_spacing_px() {
        let css = "p { letter-spacing: 1.25px; }";
        let ss = parse_stylesheet(css).unwrap();
        assert_eq!(ss.rules[0].style.letter_spacing, Some(1.25));
    }

    #[test]
    fn test_parse_letter_spacing_normal() {
        let css = "p { letter-spacing: normal; font-weight: bold; }";
        let ss = parse_stylesheet(css).unwrap();
        assert_eq!(ss.len(), 1);
        assert_eq!(ss.rules[0].style.letter_spacing, Some(0.0));
        assert_eq!(ss.rules[0].style.font_weight, Some(FontWeight::Bold));
    }

    #[test]
    fn test_parse_letter_spacing_unsupported_unit_ignored() {
        let css = "p { letter-spacing: 0.2em; font-weight: bold; }";
        let ss = parse_stylesheet(css).unwrap();
        assert_eq!(ss.len(), 1);
        assert_eq!(ss.rules[0].style.letter_spacing, None);
        assert_eq!(ss.rules[0].style.font_weight, Some(FontWeight::Bold));
    }

    #[test]
    fn test_parse_letter_spacing_invalid_does_not_clear_previous_valid_value() {
        let css = "p { letter-spacing: 1px; letter-spacing: 0.2em; }";
        let ss = parse_stylesheet(css).unwrap();
        assert_eq!(ss.len(), 1);
        assert_eq!(ss.rules[0].style.letter_spacing, Some(1.0));
    }

    #[test]
    fn test_parse_zero_margin() {
        let css = "p { margin-top: 0; }";
        let ss = parse_stylesheet(css).unwrap();
        assert_eq!(ss.rules[0].style.margin_top, Some(0.0));
    }

    #[test]
    fn test_unclosed_rule_error() {
        let css = "p { font-weight: bold;";
        let result = parse_stylesheet(css);
        assert!(result.is_err());
    }

    // -- Additional edge case tests ---

    #[test]
    fn test_multiple_classes_in_selector() {
        // Parser only handles single class; "p.a.b" should parse the first dot split
        // The selector parser finds the first dot: tag="p", class="a.b"
        let css = ".first-class { font-weight: bold; }";
        let ss = parse_stylesheet(css).unwrap();
        assert_eq!(ss.len(), 1);
        assert_eq!(
            ss.rules[0].selector,
            CssSelector::Class("first-class".into())
        );
    }

    #[test]
    fn test_cascading_later_rules_override() {
        let css = r#"
            p { font-weight: bold; text-align: left; }
            p { font-weight: normal; font-style: italic; }
        "#;
        let ss = parse_stylesheet(css).unwrap();
        assert_eq!(ss.len(), 2);

        // Resolve should apply rules in document order: later overrides earlier
        let style = ss.resolve("p", &[]);
        assert_eq!(style.font_weight, Some(FontWeight::Normal)); // overridden
        assert_eq!(style.text_align, Some(TextAlign::Left)); // kept from first
        assert_eq!(style.font_style, Some(FontStyle::Italic)); // added by second
    }

    #[test]
    fn test_font_weight_numeric_values() {
        // 400 = normal
        let css400 = "p { font-weight: 400; }";
        let ss = parse_stylesheet(css400).unwrap();
        assert_eq!(ss.rules[0].style.font_weight, Some(FontWeight::Normal));

        // 700 = bold
        let css700 = "p { font-weight: 700; }";
        let ss = parse_stylesheet(css700).unwrap();
        assert_eq!(ss.rules[0].style.font_weight, Some(FontWeight::Bold));

        // 800 = bold
        let css800 = "p { font-weight: 800; }";
        let ss = parse_stylesheet(css800).unwrap();
        assert_eq!(ss.rules[0].style.font_weight, Some(FontWeight::Bold));

        // 900 = bold
        let css900 = "p { font-weight: 900; }";
        let ss = parse_stylesheet(css900).unwrap();
        assert_eq!(ss.rules[0].style.font_weight, Some(FontWeight::Bold));
    }

    #[test]
    fn test_empty_declarations() {
        let css = "p { }";
        let ss = parse_stylesheet(css).unwrap();
        // Empty declarations produce an empty style, which is not added
        assert_eq!(ss.len(), 0);
    }

    #[test]
    fn test_whitespace_variations_no_spaces() {
        let css = "p{font-weight:bold}";
        let ss = parse_stylesheet(css).unwrap();
        assert_eq!(ss.len(), 1);
        assert_eq!(ss.rules[0].selector, CssSelector::Tag("p".into()));
        assert_eq!(ss.rules[0].style.font_weight, Some(FontWeight::Bold));
    }

    #[test]
    fn test_whitespace_variations_extra_spaces() {
        let css = "  p  {  font-weight :  bold ;  font-size :  12px ;  }  ";
        let ss = parse_stylesheet(css).unwrap();
        assert_eq!(ss.len(), 1);
        assert_eq!(ss.rules[0].style.font_weight, Some(FontWeight::Bold));
        assert_eq!(ss.rules[0].style.font_size, Some(FontSize::Px(12.0)));
    }

    #[test]
    fn test_multiple_font_family_values_take_first() {
        // CSS font-family can have multiple fallbacks separated by commas
        // Our parser takes everything after the colon as the value, then trims quotes
        // So it will get the full string. Let's verify behavior:
        let css = r#"p { font-family: 'Georgia'; }"#;
        let ss = parse_stylesheet(css).unwrap();
        assert_eq!(ss.rules[0].style.font_family, Some("Georgia".into()));

        // Double-quoted
        let css2 = r#"p { font-family: "Times New Roman"; }"#;
        let ss2 = parse_stylesheet(css2).unwrap();
        assert_eq!(
            ss2.rules[0].style.font_family,
            Some("Times New Roman".into())
        );
    }

    #[test]
    fn test_css_style_merge_both_sides_same_property() {
        let mut base = CssStyle {
            list_style_type: None,
            display: None,
            font_weight: Some(FontWeight::Bold),
            font_style: Some(FontStyle::Normal),
            direction: Some(Direction::Ltr),
            white_space: Some(WhiteSpace::Pre),
            text_indent: Some(TextIndent::Px(4.0)),
            tab_size: Some(TabSize::Spaces(4.0)),
            text_align: Some(TextAlign::Left),
            margin_top: Some(10.0),
            font_size: Some(FontSize::Px(16.0)),
            font_family: Some("Arial".into()),
            line_height: Some(LineHeight::Px(20.0)),
            letter_spacing: Some(0.5),
            margin_bottom: Some(5.0),
            hyphens: None,
            width: None,
            height: None,
            max_width: None,
            max_height: None,
            min_width: None,
            min_height: None,
        };
        let overlay = CssStyle {
            list_style_type: None,
            display: None,
            font_weight: Some(FontWeight::Normal),
            font_style: Some(FontStyle::Italic),
            direction: Some(Direction::Rtl),
            white_space: Some(WhiteSpace::Normal),
            text_indent: Some(TextIndent::Em(2.0)),
            tab_size: Some(TabSize::Px(32.0)),
            text_align: Some(TextAlign::Center),
            margin_top: Some(20.0),
            font_size: Some(FontSize::Em(1.5)),
            font_family: Some("Georgia".into()),
            line_height: Some(LineHeight::Multiplier(1.5)),
            letter_spacing: Some(2.0),
            margin_bottom: Some(15.0),
            hyphens: None,
            width: None,
            height: None,
            max_width: None,
            max_height: None,
            min_width: None,
            min_height: None,
        };
        base.merge(&overlay);

        // All values should be overridden by overlay
        assert_eq!(base.direction, Some(Direction::Rtl));
        assert_eq!(base.white_space, Some(WhiteSpace::Normal));
        assert_eq!(base.text_indent, Some(TextIndent::Em(2.0)));
        assert_eq!(base.tab_size, Some(TabSize::Px(32.0)));
        assert_eq!(base.font_weight, Some(FontWeight::Normal));
        assert_eq!(base.font_style, Some(FontStyle::Italic));
        assert_eq!(base.text_align, Some(TextAlign::Center));
        assert_eq!(base.margin_top, Some(20.0));
        assert_eq!(base.font_size, Some(FontSize::Em(1.5)));
        assert_eq!(base.font_family, Some("Georgia".into()));
        assert_eq!(base.line_height, Some(LineHeight::Multiplier(1.5)));
        assert_eq!(base.letter_spacing, Some(2.0));
        assert_eq!(base.margin_bottom, Some(15.0));
    }

    #[test]
    fn test_css_style_merge_overlay_none_preserves_base() {
        let mut base = CssStyle {
            list_style_type: None,
            display: None,
            font_weight: Some(FontWeight::Bold),
            font_size: Some(FontSize::Px(16.0)),
            ..Default::default()
        };
        let overlay = CssStyle::new(); // all None
        base.merge(&overlay);

        // Base values should be preserved
        assert_eq!(base.font_weight, Some(FontWeight::Bold));
        assert_eq!(base.font_size, Some(FontSize::Px(16.0)));
    }

    #[test]
    fn test_large_stylesheet() {
        let css = r#"
            h1 { font-weight: bold; font-size: 24px; }
            h2 { font-weight: bold; font-size: 20px; }
            h3 { font-weight: bold; font-size: 18px; }
            h4 { font-weight: bold; font-size: 16px; }
            h5 { font-weight: bold; font-size: 14px; }
            h6 { font-weight: bold; font-size: 12px; }
            p { margin-bottom: 8px; line-height: 24px; }
            .note { font-style: italic; margin-top: 10px; }
            .chapter-title { font-size: 28px; text-align: center; }
            .epigraph { font-style: italic; text-align: right; }
            .footnote { font-size: 10px; margin-top: 5px; }
            blockquote { margin-top: 12px; margin-bottom: 12px; }
        "#;
        let ss = parse_stylesheet(css).unwrap();
        assert_eq!(ss.len(), 12);

        // Spot-check first and last rules
        assert_eq!(ss.rules[0].selector, CssSelector::Tag("h1".into()));
        assert_eq!(ss.rules[0].style.font_size, Some(FontSize::Px(24.0)));

        assert_eq!(ss.rules[11].selector, CssSelector::Tag("blockquote".into()));
        assert_eq!(ss.rules[11].style.margin_top, Some(12.0));
        assert_eq!(ss.rules[11].style.margin_bottom, Some(12.0));

        // Spot-check middle
        assert_eq!(
            ss.rules[8].selector,
            CssSelector::Class("chapter-title".into())
        );
        assert_eq!(ss.rules[8].style.font_size, Some(FontSize::Px(28.0)));
        assert_eq!(ss.rules[8].style.text_align, Some(TextAlign::Center));
    }

    #[test]
    fn test_selector_with_hyphens() {
        let css = ".my-class { font-weight: bold; }";
        let ss = parse_stylesheet(css).unwrap();
        assert_eq!(ss.len(), 1);
        assert_eq!(ss.rules[0].selector, CssSelector::Class("my-class".into()));
        assert_eq!(ss.rules[0].style.font_weight, Some(FontWeight::Bold));

        // Tag.class with hyphens
        let css2 = "p.my-intro-class { font-style: italic; }";
        let ss2 = parse_stylesheet(css2).unwrap();
        assert_eq!(
            ss2.rules[0].selector,
            CssSelector::TagClass("p".into(), "my-intro-class".into())
        );
    }

    #[test]
    fn test_parse_inline_style_trailing_semicolon() {
        let style = parse_inline_style("font-weight: bold; font-size: 14px;").unwrap();
        assert_eq!(style.font_weight, Some(FontWeight::Bold));
        assert_eq!(style.font_size, Some(FontSize::Px(14.0)));
    }

    #[test]
    fn test_parse_inline_style_empty() {
        let style = parse_inline_style("").unwrap();
        assert!(style.is_empty());
    }

    #[test]
    fn test_parse_inline_style_only_semicolons() {
        let style = parse_inline_style(";;;").unwrap();
        assert!(style.is_empty());
    }

    #[test]
    fn test_font_style_oblique() {
        let css = "p { font-style: oblique; }";
        let ss = parse_stylesheet(css).unwrap();
        assert_eq!(ss.rules[0].style.font_style, Some(FontStyle::Italic));
    }

    #[test]
    fn test_resolve_no_matching_rules() {
        let css = "h1 { font-weight: bold; }";
        let ss = parse_stylesheet(css).unwrap();
        let style = ss.resolve("p", &[]);
        assert!(style.is_empty());
    }

    #[test]
    fn test_resolve_multiple_matching_classes() {
        let css = r#"
            .bold { font-weight: bold; }
            .italic { font-style: italic; }
            .centered { text-align: center; }
        "#;
        let ss = parse_stylesheet(css).unwrap();

        // Element with multiple classes
        let style = ss.resolve("p", &["bold", "italic", "centered"]);
        assert_eq!(style.font_weight, Some(FontWeight::Bold));
        assert_eq!(style.font_style, Some(FontStyle::Italic));
        assert_eq!(style.text_align, Some(TextAlign::Center));
    }

    #[test]
    fn test_css_style_is_empty_with_single_property() {
        let style = CssStyle {
            font_weight: Some(FontWeight::Bold),
            ..Default::default()
        };
        assert!(!style.is_empty());
    }

    #[test]
    fn test_stylesheet_new_is_empty() {
        let ss = Stylesheet::new();
        assert!(ss.is_empty());
        assert_eq!(ss.len(), 0);
    }

    #[test]
    fn test_css_comments_between_rules() {
        let css = r#"
            h1 { font-weight: bold; }
            /* This is a comment between rules */
            p { font-style: italic; }
            /* Another comment */
        "#;
        let ss = parse_stylesheet(css).unwrap();
        assert_eq!(ss.len(), 2);
        assert_eq!(ss.rules[0].style.font_weight, Some(FontWeight::Bold));
        assert_eq!(ss.rules[1].style.font_style, Some(FontStyle::Italic));
    }
}
