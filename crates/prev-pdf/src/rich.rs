//! A text box's rich text: runs of its text in their own styles, stored
//! as PDF rich text, the `/RC` entry's XHTML, beside `/DS`, the default
//! style the runs build on. MuPDF lays rich text out with its HTML engine
//! when it draws the box, and Acrobat edits it.

use crate::annotation::{Align, Font, Rgb, Style, TextRun};

/// The XHTML for `runs`, as `/RC` holds it: line breaks as `<br/>`, each
/// styled run a `<span>`, and every character past ASCII a numeric
/// reference, so the PDF string needs no encoding.
pub fn to_xhtml(runs: &[TextRun]) -> String {
    let mut out = String::from(
        "<?xml version=\"1.0\"?><body xmlns=\"http://www.w3.org/1999/xhtml\" \
         xmlns:xfa=\"http://www.xfa.org/schema/xfa-data/1.0/\" \
         xfa:APIVersion=\"Acrobat:11.0.0\" xfa:spec=\"2.0.2\"><p>",
    );
    for run in runs {
        let css = run_css(run);
        let mut lines = run.text.split('\n');
        let write = |out: &mut String, line: &str| {
            if line.is_empty() {
                return;
            }
            if css.is_empty() {
                escape(out, line);
            } else {
                out.push_str("<span style=\"");
                out.push_str(&css);
                out.push_str("\">");
                escape(out, line);
                out.push_str("</span>");
            }
        };
        if let Some(first) = lines.next() {
            write(&mut out, first);
        }
        for line in lines {
            out.push_str("<br/>");
            write(&mut out, line);
        }
    }
    out.push_str("</p></body>");
    out
}

/// `/DS`: the style the runs build on, from the box's own style.
pub fn default_style(style: &Style) -> String {
    let family = match style.font {
        Font::Helvetica => "Helvetica",
        Font::Times => "Times-Roman",
        Font::Courier => "Courier",
    };
    let align = match style.align {
        Align::Left => "left",
        Align::Center => "center",
        Align::Right => "right",
    };
    format!(
        "font-family:{family};font-size:{}pt;color:{};text-align:{align};",
        style.font_size,
        hex(style.text_color)
    )
}

/// The runs `/RC` holds, or `None` when it cannot be read.
pub fn from_xhtml(xhtml: &str) -> Option<Vec<TextRun>> {
    let document = roxmltree::Document::parse(xhtml).ok()?;
    let mut runs = Vec::new();
    let mut paragraphs = 0;
    walk(
        document.root_element(),
        Look::default(),
        &mut runs,
        &mut paragraphs,
    );
    Some(merge(runs))
}

/// The plain text of `runs`.
pub fn plain(runs: &[TextRun]) -> String {
    runs.iter().map(|run| run.text.as_str()).collect()
}

/// Whether `runs` style any of their text; otherwise the box's own style
/// says it all.
pub fn is_styled(runs: &[TextRun]) -> bool {
    runs.iter()
        .any(|run| run.bold || run.italic || run.underline || run.color.is_some())
}

/// `runs` carried over to `text`, changed from their text: what is kept
/// at the start and end keeps its style, and what is new takes the style
/// of the character before it, or after it at the very start, as typing
/// does. Without runs there is nothing to carry.
pub fn retext(runs: &[TextRun], text: &str) -> Vec<TextRun> {
    if runs.is_empty() {
        return Vec::new();
    }
    let looks: Vec<Look> = runs
        .iter()
        .flat_map(|run| run.text.chars().map(|_| Look::of(run)))
        .collect();
    let old: Vec<char> = runs.iter().flat_map(|run| run.text.chars()).collect();
    let new: Vec<char> = text.chars().collect();
    let prefix = old.iter().zip(&new).take_while(|(a, b)| a == b).count();
    let suffix = old[prefix..]
        .iter()
        .rev()
        .zip(new[prefix..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    let inserted = new.len() - prefix - suffix;
    let typing = if prefix > 0 {
        looks[prefix - 1]
    } else {
        looks.get(old.len() - suffix).copied().unwrap_or_default()
    };
    let mut new_looks = Vec::with_capacity(new.len());
    new_looks.extend_from_slice(&looks[..prefix]);
    new_looks.extend(std::iter::repeat_n(typing, inserted));
    new_looks.extend_from_slice(&looks[old.len() - suffix..]);
    let runs = new
        .iter()
        .zip(new_looks)
        .map(|(character, look)| look.run(character.to_string()))
        .collect();
    merge(runs)
}

/// A change to part of a text box's text.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RunChange {
    /// Bold on, or off when the whole range is bold already.
    Bold,
    Italic,
    Underline,
    /// Its own color, or the box's.
    Color(Option<Rgb>),
}

/// `runs`, over `text`, with `change` made to the bytes in `range`. Plain
/// text, without runs, becomes one run first.
pub fn restyle(
    runs: &[TextRun],
    text: &str,
    range: std::ops::Range<usize>,
    change: RunChange,
) -> Vec<TextRun> {
    let runs = if runs.is_empty() || plain(runs) != text {
        retext(
            &[TextRun {
                text: text.to_owned(),
                ..TextRun::default()
            }],
            text,
        )
    } else {
        runs.to_vec()
    };
    // Each character with its byte offset and style.
    let mut characters: Vec<(usize, char, Look)> = Vec::with_capacity(text.len());
    let mut offset = 0;
    for run in &runs {
        let look = Look::of(run);
        for character in run.text.chars() {
            characters.push((offset, character, look));
            offset += character.len_utf8();
        }
    }
    let inside = |at: usize| range.start <= at && at < range.end;
    let all = |test: fn(&Look) -> bool| {
        characters
            .iter()
            .filter(|(at, ..)| inside(*at))
            .all(|(_, _, look)| test(look))
    };
    let (bold, italic, underline) = (
        !all(|look| look.bold),
        !all(|look| look.italic),
        !all(|look| look.underline),
    );
    for (at, _, look) in &mut characters {
        if !inside(*at) {
            continue;
        }
        match change {
            RunChange::Bold => look.bold = bold,
            RunChange::Italic => look.italic = italic,
            RunChange::Underline => look.underline = underline,
            RunChange::Color(color) => look.color = color,
        }
    }
    merge(
        characters
            .into_iter()
            .map(|(_, character, look)| look.run(character.to_string()))
            .collect(),
    )
}

/// A PDF text string's bytes as text: UTF-16 or UTF-8 by their byte order
/// mark, otherwise PDFDocEncoding, which matches Latin-1 for text.
pub fn decode_text_string(bytes: &[u8]) -> String {
    if let Some(rest) = bytes.strip_prefix(&[0xfe, 0xff]) {
        let units: Vec<u16> = rest
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u16::from_be_bytes(*pair))
            .collect();
        return String::from_utf16_lossy(&units);
    }
    if let Some(rest) = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]) {
        return String::from_utf8_lossy(rest).into_owned();
    }
    bytes.iter().map(|&byte| byte as char).collect()
}

/// The styles a node's text takes from it and its ancestors.
#[derive(Clone, Copy, Default)]
struct Look {
    bold: bool,
    italic: bool,
    underline: bool,
    color: Option<Rgb>,
}

impl Look {
    fn of(run: &TextRun) -> Self {
        Self {
            bold: run.bold,
            italic: run.italic,
            underline: run.underline,
            color: run.color,
        }
    }

    fn run(self, text: String) -> TextRun {
        TextRun {
            text,
            bold: self.bold,
            italic: self.italic,
            underline: self.underline,
            color: self.color,
        }
    }
}

fn walk(
    node: roxmltree::Node<'_, '_>,
    look: Look,
    runs: &mut Vec<TextRun>,
    paragraphs: &mut usize,
) {
    if node.is_text() {
        if let Some(text) = node.text() {
            // Whitespace between block tags is layout, not text.
            if !text.trim().is_empty()
                || node
                    .parent()
                    .is_some_and(|parent| parent.tag_name().name() != "body")
            {
                runs.push(TextRun {
                    text: text.to_owned(),
                    bold: look.bold,
                    italic: look.italic,
                    underline: look.underline,
                    color: look.color,
                });
            }
        }
        return;
    }
    if !node.is_element() {
        return;
    }
    let name = node.tag_name().name();
    let mut look = look;
    match name {
        "b" | "strong" => look.bold = true,
        "i" | "em" => look.italic = true,
        "u" => look.underline = true,
        "br" => {
            runs.push(TextRun {
                text: "\n".into(),
                ..TextRun::default()
            });
            return;
        }
        _ => {}
    }
    // The body's style is the default the box's own style already gives.
    if name != "body"
        && let Some(css) = node.attribute("style")
    {
        apply_css(css, &mut look);
    }
    if name == "p" || name == "div" {
        if *paragraphs > 0 {
            runs.push(TextRun {
                text: "\n".into(),
                ..TextRun::default()
            });
        }
        *paragraphs += 1;
    }
    for child in node.children() {
        walk(child, look, runs, paragraphs);
    }
}

fn apply_css(css: &str, look: &mut Look) {
    for declaration in css.split(';') {
        let Some((property, value)) = declaration.split_once(':') else {
            continue;
        };
        let value = value.trim().to_ascii_lowercase();
        match property.trim().to_ascii_lowercase().as_str() {
            "font-weight" => {
                look.bold = value == "bold"
                    || value == "bolder"
                    || value.parse::<u32>().is_ok_and(|weight| weight >= 600);
            }
            "font-style" => look.italic = value == "italic" || value == "oblique",
            "text-decoration" => look.underline = value.contains("underline"),
            "color" => {
                if let Some(color) = parse_color(&value) {
                    look.color = Some(color);
                }
            }
            _ => {}
        }
    }
}

fn parse_color(value: &str) -> Option<Rgb> {
    let hex = value.strip_prefix('#')?;
    let digits = match hex.len() {
        3 => hex
            .chars()
            .flat_map(|digit| [digit, digit])
            .collect::<String>(),
        6 => hex.to_owned(),
        _ => return None,
    };
    let channel = |at: usize| u8::from_str_radix(&digits[at..at + 2], 16).ok();
    Some(Rgb::from_rgb8(channel(0)?, channel(2)?, channel(4)?))
}

/// Neighbouring runs in the same style as one.
fn merge(runs: Vec<TextRun>) -> Vec<TextRun> {
    let mut merged: Vec<TextRun> = Vec::new();
    for run in runs {
        match merged.last_mut() {
            Some(last) if same_look(last, &run) => last.text.push_str(&run.text),
            _ => merged.push(run),
        }
    }
    merged
}

fn same_look(a: &TextRun, b: &TextRun) -> bool {
    a.bold == b.bold && a.italic == b.italic && a.underline == b.underline && a.color == b.color
}

fn run_css(run: &TextRun) -> String {
    let mut css = String::new();
    if run.bold {
        css.push_str("font-weight:bold;");
    }
    if run.italic {
        css.push_str("font-style:italic;");
    }
    if run.underline {
        css.push_str("text-decoration:underline;");
    }
    if let Some(color) = run.color {
        css.push_str("color:");
        css.push_str(&hex(color));
        css.push(';');
    }
    css
}

fn hex(color: Rgb) -> String {
    let byte = |value: f32| (value.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!(
        "#{:02x}{:02x}{:02x}",
        byte(color.red),
        byte(color.green),
        byte(color.blue)
    )
}

fn escape(out: &mut String, text: &str) {
    for character in text.chars() {
        match character {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            character if character.is_ascii() => out.push(character),
            character => {
                out.push_str("&#");
                out.push_str(&(character as u32).to_string());
                out.push(';');
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(text: &str) -> TextRun {
        TextRun {
            text: text.into(),
            ..TextRun::default()
        }
    }

    #[test]
    fn runs_survive_the_xhtml() {
        let runs = vec![
            run("Plain, "),
            TextRun {
                bold: true,
                ..run("bold")
            },
            run(" and "),
            TextRun {
                color: Some(Rgb::from_rgb8(255, 0, 0)),
                italic: true,
                ..run("red italic")
            },
            run("\nnext line, café ✓ <&>"),
        ];
        let xhtml = to_xhtml(&runs);
        assert!(xhtml.is_ascii(), "{xhtml}");
        assert_eq!(from_xhtml(&xhtml), Some(runs));
    }

    #[test]
    fn acrobat_rich_text_reads() {
        let xhtml = "<?xml version=\"1.0\"?><body xmlns=\"http://www.w3.org/1999/xhtml\" \
            style=\"font-size:12pt;color:#000000\"><p dir=\"ltr\">One <b>two</b></p>\
            <p dir=\"ltr\"><span style=\"color:#00f;font-style:italic\">three</span></p></body>";
        let runs = from_xhtml(xhtml).unwrap();
        assert_eq!(plain(&runs), "One two\nthree");
        assert!(runs.iter().any(|run| run.bold && run.text == "two"));
        assert!(runs.iter().any(|run| {
            run.italic && run.text == "three" && run.color == Some(Rgb::from_rgb8(0, 0, 255))
        }));
    }

    #[test]
    fn retext_keeps_styles_around_an_edit() {
        let runs = vec![
            run("Hello "),
            TextRun {
                bold: true,
                ..run("world")
            },
        ];
        // Typed inside the bold word: bold too.
        let typed = retext(&runs, "Hello wonderful world");
        assert_eq!(plain(&typed), "Hello wonderful world");
        assert!(
            typed
                .iter()
                .any(|run| run.bold && run.text == "wonderful world")
        );
        assert!(typed.iter().any(|run| !run.bold && run.text == "Hello "));
        // Typed at the very start: the first character's style.
        let start = retext(&runs, "Oh, Hello world");
        assert_eq!(start[0].text, "Oh, Hello ");
        assert!(!start[0].bold);
        // Deleted across the styles: what stays keeps its own.
        let cut = retext(&runs, "Help");
        assert_eq!(plain(&cut), "Help");
        assert!(retext(&[], "anything").is_empty());
    }

    #[test]
    fn restyle_toggles_and_colors_a_range() {
        let text = "one two three";
        // Plain text: "two" goes bold.
        let bold = restyle(&[], text, 4..7, RunChange::Bold);
        assert_eq!(plain(&bold), text);
        assert!(bold.iter().any(|run| run.bold && run.text == "two"));
        // All bold already: off again, back to one plain run.
        let unbold = restyle(&bold, text, 4..7, RunChange::Bold);
        assert!(!is_styled(&unbold));
        // Partly bold: the whole range goes bold.
        let wider = restyle(&bold, text, 0..7, RunChange::Bold);
        assert!(wider.iter().any(|run| run.bold && run.text == "one two"));
        // A color over part of a bold run splits it.
        let red = Rgb::from_rgb8(255, 0, 0);
        let colored = restyle(&wider, text, 4..13, RunChange::Color(Some(red)));
        assert!(
            colored
                .iter()
                .any(|run| run.bold && run.color == Some(red) && run.text == "two")
        );
        assert!(
            colored
                .iter()
                .any(|run| !run.bold && run.color == Some(red) && run.text == " three")
        );
    }

    #[test]
    fn text_strings_decode_by_their_mark() {
        assert_eq!(
            decode_text_string(&[0xfe, 0xff, 0x00, 0x41, 0x00, 0xe9]),
            "Aé"
        );
        assert_eq!(decode_text_string("é".as_bytes()), "Ã©");
        assert_eq!(decode_text_string(&[0xef, 0xbb, 0xbf, 0xc3, 0xa9]), "é");
    }
}
