//! Rendered hit regions are the single source of truth for mouse targeting.
use crossterm::event::KeyCode;
use ratatui::{
    layout::Rect,
    style::{Color, Style},
    widgets::Paragraph,
    Frame,
};
use std::{
    cell::{Cell, RefCell},
    time::Instant,
};
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    Key(KeyCode),
    Select(usize),
    List(usize),
    Address(u64),
    Page(u64),
    Setting(usize),
    Scroll(usize),
}
#[derive(Default)]
pub struct MouseState {
    pub hits: RefCell<Vec<(Rect, Action)>>,
    pub last: Option<(Instant, Action, Vec<usize>)>,
    pub dragging: bool,
    pub scrolls: [Cell<u16>; 4],
}
impl MouseState {
    pub fn add(&self, rect: Rect, action: Action) {
        self.hits.borrow_mut().push((rect, action));
    }
    pub fn hit(&self, x: u16, y: u16) -> Option<Action> {
        self.hits
            .borrow()
            .iter()
            .rev()
            .find(|(r, _)| r.contains((x, y).into()))
            .map(|(_, a)| a.clone())
    }
}
fn button_text(label: &str, action: &Action) -> String {
    let key = match action {
        Action::Key(KeyCode::Char(' ')) => "Space".into(),
        Action::Key(KeyCode::Char(c)) => c.to_string(),
        Action::Key(KeyCode::Backspace) => "Backspace".into(),
        Action::Key(KeyCode::Esc) => "Esc".into(),
        Action::Key(KeyCode::Enter) => "Enter".into(),
        Action::Key(KeyCode::Tab) => "Tab".into(),
        _ => return format!("[{label}] "),
    };
    format!("[{label} {key}] ")
}

pub(crate) fn button_rows(width: u16, items: &[(&str, Action)]) -> u16 {
    use unicode_width::UnicodeWidthStr;
    let mut used = 0;
    let mut rows = 1;
    for (label, action) in items {
        let w = button_text(label, action).width() as u16;
        if used > 0 && used + w > width {
            rows += 1;
            used = 0;
        }
        used += w;
    }
    rows
}

pub fn buttons(frame: &mut Frame, area: Rect, items: &[(&str, Action)], state: &MouseState) {
    use unicode_width::UnicodeWidthStr;
    let mut x = area.x;
    let mut y = area.y;
    for (label, action) in items {
        let text = button_text(label, action);
        let width = text.width() as u16;
        if x != area.x && x.saturating_add(width) > area.right() {
            x = area.x;
            y += 1;
        }
        if y >= area.bottom() || width > area.width {
            break;
        }
        let rect = Rect::new(x, y, width, 1);
        frame.render_widget(
            Paragraph::new(text).style(Style::default().fg(Color::Cyan)),
            rect,
        );
        state.add(rect, action.clone());
        x += width;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ui, App};
    use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
    fn send(app: &mut App, kind: MouseEventKind, rect: Rect) {
        app.handle_mouse(MouseEvent {
            kind,
            column: rect.x,
            row: rect.y,
            modifiers: KeyModifiers::NONE,
        });
    }
    #[test]
    fn rendered_regions_select_drag_drill_and_modal_isolation() {
        let mut app = App::new(tmv_core::demo::snapshot(), None, true, false);
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(140, 40)).unwrap();
        term.draw(|f| ui::draw(f, &mut app)).unwrap();
        let hits = app.mouse.hits.borrow().clone();
        let (first, _) = hits.iter().find(|(_, a)| *a == Action::Select(0)).unwrap();
        let (second, _) = hits.iter().find(|(_, a)| *a == Action::Select(1)).unwrap();
        send(&mut app, MouseEventKind::Down(MouseButton::Left), *first);
        send(&mut app, MouseEventKind::Drag(MouseButton::Left), *second);
        assert_eq!(app.selected, 1);
        send(&mut app, MouseEventKind::Up(MouseButton::Left), *second);
        send(&mut app, MouseEventKind::Down(MouseButton::Left), *first);
        send(&mut app, MouseEventKind::Down(MouseButton::Left), *first);
        assert_eq!(app.focus, vec![0]);
        send(&mut app, MouseEventKind::Down(MouseButton::Right), *first);
        assert!(app.focus.is_empty());
        assert!(!app.should_quit);
        app.show_settings = true;
        term.draw(|f| ui::draw(f, &mut app)).unwrap();
        assert!(!app
            .mouse
            .hits
            .borrow()
            .iter()
            .any(|(_, a)| matches!(a, Action::Select(_))));
        let (close, _) = app
            .mouse
            .hits
            .borrow()
            .iter()
            .find(|(_, a)| *a == Action::Key(KeyCode::Esc))
            .unwrap()
            .clone();
        send(&mut app, MouseEventKind::Down(MouseButton::Left), close);
        assert!(!app.show_settings);
    }
    #[test]
    fn help_click_dispatches_action_after_closing_menu() {
        let mut app = App::new(tmv_core::demo::snapshot(), None, true, false);
        app.show_help = true;
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(140, 40)).unwrap();
        term.draw(|f| ui::draw(f, &mut app)).unwrap();
        let (rect, _) = app
            .mouse
            .hits
            .borrow()
            .iter()
            .find(|(_, a)| *a == Action::Key(KeyCode::Char('s')))
            .unwrap()
            .clone();
        send(&mut app, MouseEventKind::Down(MouseButton::Left), rect);
        assert!(!app.show_help);
        assert!(app.show_settings);
    }
}
