use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};
use termimad::MadSkin;

use crate::tui::app::{App, SidebarSection, TextSelection};
use crate::tui::data::DailyChatMessage;
use crate::tui::event::{Focus, PaneRects};
use crate::tui::markdown::render_markdown;

pub fn draw(frame: &mut Frame<'_>, app: &mut App, skin: &MadSkin) {
    let size = frame.area();
    let vchunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(3),
            Constraint::Length(1),
        ])
        .split(size);

    draw_header(frame, vchunks[0], app);
    let body = vchunks[1];
    let hchunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Ratio(1, 6),
            Constraint::Ratio(2, 6),
            Constraint::Ratio(3, 6),
        ])
        .split(body);
    let sidebar_area = hchunks[0];
    let middle_area = hchunks[1];
    let right_area = hchunks[2];

    app.pane_rects = PaneRects {
        sidebar: sidebar_area,
        middle: middle_area,
        right: right_area,
        sidebar_list_top: sidebar_area.y.saturating_add(1),
        middle_list_top: middle_area.y.saturating_add(1),
        right_content_top: right_area.y.saturating_add(1),
    };

    draw_sidebar(frame, sidebar_area, app);
    draw_middle(frame, middle_area, app);
    draw_right(frame, right_area, app, skin);
    // A live confirm prompt outranks transient status (and survives the
    // status auto-clear timer) so the modal choice is always visible.
    let prompt = app.confirm_prompt();
    let footer_msg = prompt.as_deref().or(app.status.as_deref());
    draw_footer(frame, vchunks[2], app.is_editing(), footer_msg);
}

fn focused_border(focus: Focus, pane: Focus) -> Style {
    if focus == pane {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    }
}

fn draw_header(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let user_segment = match &app.user {
        Some(u) => {
            let label = match (&u.name, &u.email) {
                (Some(n), Some(e)) if n != e => format!("{n} <{e}>"),
                (Some(n), _) => n.clone(),
                (None, Some(e)) => e.clone(),
                (None, None) if !u.id.is_empty() => format!("id:{}", u.id),
                _ => "unknown".to_string(),
            };
            format!("   user: {label}")
        }
        None => "   user: (not signed in)".to_string(),
    };
    let text = format!(
        " dayone tui   profile: {}{} ",
        app.profile_name, user_segment
    );
    let p = Paragraph::new(text).style(
        Style::default()
            .fg(Color::Black)
            .bg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    );
    frame.render_widget(p, area);
}

fn draw_footer(frame: &mut Frame<'_>, area: Rect, editing: bool, status: Option<&str>) {
    // A pending status message takes over the footer (in red) so it never
    // overpaints entry content; otherwise show the context-appropriate hint.
    if let Some(status) = status {
        let p = Paragraph::new(format!(" {status} ")).style(Style::default().fg(Color::Red));
        frame.render_widget(p, area);
        return;
    }
    let hint = if editing {
        " EDIT · arrows move · Shift+arrows select · Ctrl+Y copy · Ctrl+S save · Esc exit "
    } else {
        " ↑↓/jk move · Enter edit · → preview · Ctrl+N new · Ctrl+D delete · Esc back · Tab switch · Ctrl+Y copy · Ctrl+C quit "
    };
    let p = Paragraph::new(hint).style(Style::default().fg(Color::DarkGray));
    frame.render_widget(p, area);
}

fn draw_sidebar(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let border = focused_border(app.focus, Focus::Sidebar);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(Span::styled(" Journals ", Style::default().fg(Color::Cyan)))
        .border_style(border);

    let mut items: Vec<ListItem> = app
        .journals
        .iter()
        .enumerate()
        .map(|(i, j)| {
            let dot_color = j
                .color
                .as_deref()
                .and_then(parse_hex_color)
                .unwrap_or(Color::DarkGray);
            let dot = if j.color.is_some() { "● " } else { "  " };
            let line = Line::from(vec![
                Span::styled(dot, Style::default().fg(dot_color)),
                Span::raw(j.name.clone()),
            ]);
            let style = if matches!(app.sidebar, SidebarSection::Journal(k) if k == i) {
                Style::default()
                    .add_modifier(Modifier::REVERSED)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            ListItem::new(line).style(style)
        })
        .collect();

    let chat_selected = matches!(app.sidebar, SidebarSection::DailyChat);
    let chat_style = if chat_selected {
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::REVERSED)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Yellow)
    };
    // A dim horizontal rule separates journals from the DAILY CHAT entry
    // point. Rendered as the first line of the DAILY CHAT ListItem so the
    // item still maps to one logical sidebar index.
    let divider_width = area.width.saturating_sub(2).max(1) as usize;
    let divider_line = Line::from(Span::styled(
        "─".repeat(divider_width),
        Style::default().fg(Color::DarkGray),
    ));
    let label_line = Line::from(vec![
        Span::raw("  "),
        Span::styled("DAILY CHAT", Style::default().fg(Color::Yellow)),
    ])
    .style(chat_style);
    items.push(ListItem::new(Text::from(vec![divider_line, label_line])));

    let list = List::new(items).block(block);
    frame.render_widget(list, area);
}

fn draw_middle(frame: &mut Frame<'_>, area: Rect, app: &mut App) {
    let border = focused_border(app.focus, Focus::Middle);
    app.middle_row_heights.clear();
    match app.sidebar {
        SidebarSection::Journal(i) => {
            let Some(j) = app.journals.get(i).cloned() else {
                render_empty(frame, area, border, " Entries ", "No journal selected.");
                return;
            };
            let Some(entries) = app.entries_by_journal.get(&j.id).cloned() else {
                render_empty(
                    frame,
                    area,
                    border,
                    " Entries ",
                    "Press Enter or select to load entries.",
                );
                return;
            };
            if entries.is_empty() {
                render_empty(
                    frame,
                    area,
                    border,
                    " Entries ",
                    "No entries in this journal.",
                );
                return;
            }
            let selected = *app.middle_index_entries.get(&j.id).unwrap_or(&0);
            let inner_width = area.width.saturating_sub(4) as usize; // borders + highlight symbol
            let last_index = entries.len().saturating_sub(1);
            let highlight = Style::default()
                .add_modifier(Modifier::REVERSED)
                .add_modifier(Modifier::BOLD);
            app.middle_row_heights.reserve(entries.len());
            let rows: Vec<ListItem> = entries
                .iter()
                .enumerate()
                .map(|(i, e)| {
                    let is_selected = i == selected;
                    let mut lines: Vec<Line<'static>> = Vec::with_capacity(4);
                    // Line 1: date.
                    let date_text = if e.entry_date.is_empty() {
                        "(no date)".to_string()
                    } else {
                        e.entry_date.clone()
                    };
                    let mut date_line = Line::from(Span::styled(
                        date_text,
                        Style::default()
                            .fg(Color::Blue)
                            .add_modifier(Modifier::BOLD),
                    ));
                    if is_selected {
                        date_line = date_line.style(highlight);
                    }
                    lines.push(date_line);
                    // Line 2: preview.
                    let mut preview_line =
                        Line::from(Span::raw(truncate_to_width(&e.preview, inner_width)));
                    if is_selected {
                        preview_line = preview_line.style(highlight);
                    }
                    lines.push(preview_line);
                    // Line 3: tags · media — skipped entirely when neither is present.
                    if !e.tags.is_empty() || e.media_count > 0 {
                        let mut meta_line = Line::from(render_meta_spans(e, inner_width));
                        if is_selected {
                            meta_line = meta_line.style(highlight);
                        }
                        lines.push(meta_line);
                    }
                    // Divider (dim, never highlighted; skipped after the last entry).
                    if i < last_index {
                        lines.push(Line::from(Span::styled(
                            "─".repeat(inner_width.max(1)),
                            Style::default().fg(Color::DarkGray),
                        )));
                    }
                    app.middle_row_heights.push(lines.len() as u16);
                    ListItem::new(Text::from(lines))
                })
                .collect();

            let block = Block::default()
                .borders(Borders::ALL)
                .title(Span::styled(" Entries ", Style::default().fg(Color::Cyan)))
                .border_style(border);
            let list = List::new(rows).block(block).highlight_symbol("▸ ");
            let mut state = ListState::default();
            state.select(Some(selected));
            frame.render_stateful_widget(list, area, &mut state);
        }
        SidebarSection::DailyChat => {
            if app.daily_chat_days.is_empty() {
                render_empty(
                    frame,
                    area,
                    border,
                    " Daily Chat ",
                    "No daily chat history found.",
                );
                return;
            }
            app.middle_row_heights
                .extend(std::iter::repeat_n(1u16, app.daily_chat_days.len()));
            let rows: Vec<ListItem> = app
                .daily_chat_days
                .iter()
                .map(|d| {
                    let line = Line::from(vec![
                        Span::styled(format!("{:<10} ", d.date), Style::default().fg(Color::Blue)),
                        Span::styled(
                            format!("({} msgs)", d.message_count),
                            Style::default().fg(Color::DarkGray),
                        ),
                    ]);
                    ListItem::new(line)
                })
                .collect();
            render_list(
                frame,
                area,
                border,
                " Daily Chat ",
                rows,
                app.middle_index_chat,
            );
        }
    }
}

fn render_list(
    frame: &mut Frame<'_>,
    area: Rect,
    border: Style,
    title: &str,
    rows: Vec<ListItem<'_>>,
    selected: usize,
) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(Span::styled(
            title.to_string(),
            Style::default().fg(Color::Cyan),
        ))
        .border_style(border);
    let list = List::new(rows)
        .block(block)
        .highlight_style(
            Style::default()
                .add_modifier(Modifier::REVERSED)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▸ ");
    let mut state = ListState::default();
    state.select(Some(selected));
    frame.render_stateful_widget(list, area, &mut state);
}

fn render_empty(frame: &mut Frame<'_>, area: Rect, border: Style, title: &str, msg: &str) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(Span::styled(
            title.to_string(),
            Style::default().fg(Color::Cyan),
        ))
        .border_style(border);
    let p = Paragraph::new(msg)
        .block(block)
        .style(Style::default().fg(Color::DarkGray))
        .wrap(Wrap { trim: true });
    frame.render_widget(p, area);
}

fn draw_right(frame: &mut Frame<'_>, area: Rect, app: &mut App, skin: &MadSkin) {
    if app.editor.is_some() {
        draw_editor(frame, area, app);
        return;
    }
    let border = focused_border(app.focus, Focus::Right);
    let (title, text) = match app.sidebar {
        SidebarSection::Journal(i) => {
            let Some(j) = app.journals.get(i) else {
                render_empty(frame, area, border, " Content ", "No journal.");
                return;
            };
            let entries = match app.entries_by_journal.get(&j.id) {
                Some(e) if !e.is_empty() => e,
                _ => {
                    render_empty(frame, area, border, " Content ", "Select an entry.");
                    return;
                }
            };
            let idx = *app.middle_index_entries.get(&j.id).unwrap_or(&0);
            let Some(entry) = entries.get(idx) else {
                render_empty(frame, area, border, " Content ", "Select an entry.");
                return;
            };
            match app.entry_bodies.peek(&entry.id) {
                Some(body) => {
                    let width = area.width.saturating_sub(2) as usize;
                    let rendered = render_markdown(skin, &body.body_markdown, width.max(20));
                    (
                        format!(
                            " {} — {} ",
                            entry.entry_date,
                            truncate_title(&entry.preview, 40)
                        ),
                        rendered,
                    )
                }
                None => (format!(" {} ", entry.entry_date), Text::from("Loading…")),
            }
        }
        SidebarSection::DailyChat => {
            let Some(day) = app.daily_chat_days.get(app.middle_index_chat) else {
                render_empty(frame, area, border, " Chat ", "Select a date.");
                return;
            };
            match app.daily_chat_messages.get(&day.id) {
                Some(msgs) if !msgs.is_empty() => {
                    let width = area.width.saturating_sub(2) as usize;
                    (
                        format!(" Chat — {} ", day.date),
                        render_chat(skin, msgs, width.max(20)),
                    )
                }
                Some(_) => (format!(" Chat — {} ", day.date), Text::from("No messages.")),
                None => (format!(" Chat — {} ", day.date), Text::from("Loading…")),
            }
        }
    };

    // Clamp scrolling to actual overflow: markdown/chat text is already wrapped
    // to the inner width, so each line maps to one screen row. When everything
    // fits, max_scroll is 0 and arrow-key scrolling is effectively disabled.
    let inner_height = area.height.saturating_sub(2) as usize;
    let max_scroll = text.lines.len().saturating_sub(inner_height) as u16;
    if app.right_scroll > max_scroll {
        app.right_scroll = max_scroll;
    }

    // Stash the plain rendered lines so mouse selection can map screen
    // coordinates back to characters, then apply any active selection highlight.
    app.right_rendered_lines = text_to_plain_lines(&text);
    let text = match app.right_selection {
        Some(selection) => highlight_selection(text, selection),
        None => text,
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(Span::styled(title, Style::default().fg(Color::Cyan)))
        .border_style(border);
    let p = Paragraph::new(text)
        .block(block)
        .wrap(Wrap { trim: false })
        .scroll((app.right_scroll, 0));
    frame.render_widget(p, area);
}

fn draw_editor(frame: &mut Frame<'_>, area: Rect, app: &mut App) {
    let title = match current_entry_date(app) {
        Some(date) => format!(" Editing — {date} "),
        None => " Editing ".to_string(),
    };
    let border = Style::default()
        .fg(Color::Yellow)
        .add_modifier(Modifier::BOLD);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(Span::styled(title, Style::default().fg(Color::Yellow)))
        .border_style(border);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let inner_width = inner.width.max(1) as usize;
    let inner_height = inner.height;

    let ed = app.editor.as_mut().expect("editor present");
    ed.wrap_width = inner_width;
    ed.clamp_scroll(inner_height, inner_width);

    let rows = ed.layout(inner_width);
    let selection = ed.selection();
    let sel_style = Style::default()
        .add_modifier(Modifier::REVERSED)
        .add_modifier(Modifier::BOLD);

    let mut lines: Vec<Line<'static>> = Vec::with_capacity(rows.len());
    for row in &rows {
        let chars = ed.line_chars(row.line);
        let mut spans: Vec<Span<'static>> = Vec::new();
        let mut run = String::new();
        let mut run_selected = false;
        for offset in 0..row.len {
            let col = row.start + offset;
            let ch = chars.get(col).copied().unwrap_or(' ');
            let pos = crate::tui::editor::Pos {
                line: row.line,
                col,
            };
            let selected = selection.map(|(s, e)| pos >= s && pos < e).unwrap_or(false);
            if selected != run_selected && !run.is_empty() {
                spans.push(styled_run(
                    std::mem::take(&mut run),
                    run_selected,
                    sel_style,
                ));
            }
            run_selected = selected;
            run.push(ch);
        }
        if !run.is_empty() {
            spans.push(styled_run(run, run_selected, sel_style));
        }
        lines.push(Line::from(spans));
    }

    let scroll = ed.scroll;
    let cursor_visual_row = ed.cursor_visual_row(&rows, inner_width);
    let (_, cursor_col) = ed.cursor_screen(&rows, inner_width);

    let paragraph = Paragraph::new(Text::from(lines)).scroll((scroll, 0));
    frame.render_widget(paragraph, inner);

    // Place the terminal cursor when its row is within the visible window.
    if (cursor_visual_row as u16) >= scroll {
        let rel_row = cursor_visual_row as u16 - scroll;
        if rel_row < inner_height {
            let cx = inner.x + (cursor_col as u16).min(inner.width.saturating_sub(1));
            let cy = inner.y + rel_row;
            frame.set_cursor_position((cx, cy));
        }
    }
}

fn styled_run(text: String, selected: bool, sel_style: Style) -> Span<'static> {
    if selected {
        Span::styled(text, sel_style)
    } else {
        Span::raw(text)
    }
}

fn current_entry_date(app: &App) -> Option<String> {
    if let SidebarSection::Journal(i) = app.sidebar {
        let j = app.journals.get(i)?;
        let entries = app.entries_by_journal.get(&j.id)?;
        let idx = *app.middle_index_entries.get(&j.id).unwrap_or(&0);
        return entries.get(idx).map(|e| e.entry_date.clone());
    }
    None
}

fn text_to_plain_lines(text: &Text<'_>) -> Vec<String> {
    text.lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .collect()
}

fn highlight_selection(text: Text<'static>, selection: TextSelection) -> Text<'static> {
    let ((start_line, start_col), (end_line, end_col)) = selection.normalized();
    let highlight = Style::default()
        .fg(Color::Black)
        .bg(Color::Yellow)
        .add_modifier(Modifier::BOLD);
    let lines: Vec<Line<'static>> = text
        .lines
        .into_iter()
        .enumerate()
        .map(|(line_idx, line)| {
            let line_idx = line_idx as u16;
            if line_idx < start_line || line_idx > end_line {
                return line;
            }
            let start = if line_idx == start_line { start_col } else { 0 };
            let end = if line_idx == end_line {
                end_col
            } else {
                line_width(&line) as u16
            };
            highlight_line(line, start, end, highlight)
        })
        .collect();
    Text::from(lines)
}

fn line_width(line: &Line<'_>) -> usize {
    use unicode_width::UnicodeWidthStr;
    line.spans
        .iter()
        .map(|span| span.content.as_ref().width())
        .sum()
}

fn highlight_line(
    line: Line<'static>,
    start_col: u16,
    end_col: u16,
    highlight: Style,
) -> Line<'static> {
    use unicode_width::UnicodeWidthChar;
    if start_col == end_col {
        return line;
    }
    let mut col = 0u16;
    let mut spans = Vec::new();
    for span in line.spans {
        let mut chunk = String::new();
        let mut selected = false;
        let base_style = span.style;
        for ch in span.content.chars() {
            let width = ch.width().unwrap_or(0) as u16;
            let next = col.saturating_add(width);
            let is_selected = next > start_col && col < end_col;
            if !chunk.is_empty() && is_selected != selected {
                let style = if selected {
                    base_style.patch(highlight)
                } else {
                    base_style
                };
                spans.push(Span::styled(std::mem::take(&mut chunk), style));
            }
            selected = is_selected;
            chunk.push(ch);
            col = next;
        }
        if !chunk.is_empty() {
            let style = if selected {
                base_style.patch(highlight)
            } else {
                base_style
            };
            spans.push(Span::styled(chunk, style));
        }
    }
    Line::from(spans)
}

fn render_chat(skin: &MadSkin, msgs: &[DailyChatMessage], width: usize) -> Text<'static> {
    let mut lines: Vec<Line<'static>> = Vec::new();
    for (i, m) in msgs.iter().enumerate() {
        if i > 0 {
            lines.push(Line::from(""));
        }
        let role_color = if m.role == "assistant" {
            Color::Green
        } else {
            Color::Magenta
        };
        let header = match &m.created_at {
            Some(ts) => format!("{} • {}", m.role, ts),
            None => m.role.clone(),
        };
        lines.push(Line::from(Span::styled(
            header,
            Style::default().fg(role_color).add_modifier(Modifier::BOLD),
        )));
        let rendered = render_markdown(skin, &m.text, width);
        for l in rendered.lines {
            lines.push(l);
        }
    }
    Text::from(lines)
}

fn truncate_title(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let take = max.saturating_sub(1);
    let mut out: String = s.chars().take(take).collect();
    out.push('…');
    out
}

fn parse_hex_color(raw: &str) -> Option<Color> {
    let hex = raw.trim().trim_start_matches('#');
    // Must be pure ASCII hex; otherwise byte indexing below would be
    // misaligned on multibyte input and could panic.
    if !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    match hex.len() {
        6 => {
            let r = u8::from_str_radix(hex.get(0..2)?, 16).ok()?;
            let g = u8::from_str_radix(hex.get(2..4)?, 16).ok()?;
            let b = u8::from_str_radix(hex.get(4..6)?, 16).ok()?;
            Some(Color::Rgb(r, g, b))
        }
        3 => {
            let r = u8::from_str_radix(hex.get(0..1)?, 16).ok()?;
            let g = u8::from_str_radix(hex.get(1..2)?, 16).ok()?;
            let b = u8::from_str_radix(hex.get(2..3)?, 16).ok()?;
            Some(Color::Rgb(r * 17, g * 17, b * 17))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::parse_hex_color;
    use ratatui::style::Color;

    #[test]
    fn parses_six_digit_hex() {
        assert_eq!(
            parse_hex_color("#44c0ff"),
            Some(Color::Rgb(0x44, 0xc0, 0xff))
        );
        assert_eq!(
            parse_hex_color("44C0FF"),
            Some(Color::Rgb(0x44, 0xc0, 0xff))
        );
    }

    #[test]
    fn parses_three_digit_shorthand() {
        assert_eq!(parse_hex_color("#f0a"), Some(Color::Rgb(0xff, 0x00, 0xaa)));
    }

    #[test]
    fn rejects_bad_input() {
        assert_eq!(parse_hex_color(""), None);
        assert_eq!(parse_hex_color("#gg0000"), None);
        assert_eq!(parse_hex_color("#12345"), None);
    }

    #[test]
    fn rejects_non_ascii_without_panicking() {
        // Multibyte characters used to panic via byte slicing on non-char
        // boundaries; now they return None cleanly.
        assert_eq!(parse_hex_color("#日本語"), None);
        assert_eq!(parse_hex_color("héllo!"), None);
    }
}

fn truncate_to_width(s: &str, max: usize) -> String {
    use unicode_width::UnicodeWidthChar;
    if max == 0 {
        return String::new();
    }
    let total: usize = s.chars().map(|c| c.width().unwrap_or(0)).sum();
    if total <= max {
        return s.to_string();
    }
    if max == 1 {
        return "…".to_string();
    }
    let budget = max.saturating_sub(1);
    let mut used = 0usize;
    let mut out = String::new();
    for ch in s.chars() {
        let w = ch.width().unwrap_or(0);
        if used + w > budget {
            break;
        }
        used += w;
        out.push(ch);
    }
    out.push('…');
    out
}

fn render_meta_spans(e: &crate::tui::data::EntryPreview, max_width: usize) -> Vec<Span<'static>> {
    let has_tags = !e.tags.is_empty();
    let has_media = e.media_count > 0;
    if !has_tags && !has_media {
        return vec![Span::styled(
            "—".to_string(),
            Style::default().fg(Color::DarkGray),
        )];
    }
    let mut spans: Vec<Span<'static>> = Vec::new();
    if has_tags {
        let joined = e
            .tags
            .iter()
            .map(|t| format!("#{t}"))
            .collect::<Vec<_>>()
            .join(" ");
        let trimmed = truncate_to_width(&joined, max_width.saturating_sub(10));
        spans.push(Span::styled(trimmed, Style::default().fg(Color::Magenta)));
    }
    if has_media {
        if has_tags {
            spans.push(Span::styled(
                "  ·  ".to_string(),
                Style::default().fg(Color::DarkGray),
            ));
        }
        let label = if e.media_count == 1 {
            "1 media".to_string()
        } else {
            format!("{} media", e.media_count)
        };
        spans.push(Span::styled(label, Style::default().fg(Color::Green)));
    }
    spans
}
