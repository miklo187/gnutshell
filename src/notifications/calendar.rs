//! The month view beside the notification list. Month and day names and the
//! first day of the week follow the locale. It shows dates only, no events.
//!
//! Weekdays are numbered from 0 for Monday to 6 for Sunday.

use super::{Notifications, PADDING, SPACING, SUMMARY_FONT, with};
use crate::ffi::{self, ClutterActor};
use crate::ui::{self, gobj};
use gio::prelude::*;
use std::ffi::{CStr, c_char, c_int};
use std::rc::Rc;

const CELL_WIDTH: f32 = 40.0;
const CELL_HEIGHT: f32 = 32.0;
/// Room for week numbers.
const WEEK_WIDTH: f32 = 30.0;
const ROW_HEIGHT: f32 = 32.0;
const ARROW_WIDTH: f32 = 36.0;
/// Always as many, so that the size does not change from month to month.
const WEEKS: usize = 6;
const DATE_FONT: &CStr = c"Sans Bold 12";
const TODAY_COLOR: ffi::CoglColor = ui::SELECTED_COLOR;
const HOVER_COLOR: ffi::CoglColor = ffi::CoglColor::rgba(0xff, 0xff, 0xff, 0x20);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Date {
    pub year: i32,
    pub month: i32,
    pub day: i32,
}

fn days_in_month(year: i32, month: i32) -> i32 {
    match month {
        4 | 6 | 9 | 11 => 30,
        2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
        2 => 28,
        _ => 31,
    }
}

/// The weekday of a date in the Gregorian calendar (Sakamoto's method).
fn weekday(date: Date) -> i32 {
    const SHIFT: [i32; 12] = [0, 3, 2, 5, 0, 3, 5, 1, 4, 6, 2, 4];
    let year = if date.month < 3 { date.year - 1 } else { date.year };
    let sunday_based =
        (year + year / 4 - year / 100 + year / 400 + SHIFT[(date.month - 1) as usize] + date.day) % 7;
    (sunday_based + 6) % 7
}

/// The year and month `delta` months from the given ones.
fn add_months(year: i32, month: i32, delta: i32) -> (i32, i32) {
    let months = year * 12 + month - 1 + delta;
    (months.div_euclid(12), months.rem_euclid(12) + 1)
}

/// The days shown for a month: whole weeks starting on `week_start`, filled up
/// with days of the months before and after.
pub fn grid(year: i32, month: i32, week_start: i32) -> Vec<Date> {
    let before = (weekday(Date { year, month, day: 1 }) - week_start).rem_euclid(7);
    let (previous_year, previous) = add_months(year, month, -1);
    let (next_year, next) = add_months(year, month, 1);
    let length = days_in_month(year, month);
    let previous_length = days_in_month(previous_year, previous);
    (0..7 * WEEKS as i32)
        .map(|cell| {
            let day = cell - before + 1;
            if day < 1 {
                Date { year: previous_year, month: previous, day: previous_length + day }
            } else if day > length {
                Date { year: next_year, month: next, day: day - length }
            } else {
                Date { year, month, day }
            }
        })
        .collect()
}

/// The weekday from glibc's description of a week: `origin`, a date written as
/// 19971130, is day 1 of the locale's week, and `first` the day it starts on.
fn start_from_locale(origin: u32, first: u8) -> i32 {
    let date = Date { year: (origin / 10000) as i32, month: (origin / 100 % 100) as i32, day: (origin % 100) as i32 };
    if !(1..=12).contains(&date.month) || !(1..=7).contains(&first) {
        return 0;
    }
    (weekday(date) + i32::from(first) - 1) % 7
}

/// The day weeks start on in the `LC_TIME` locale.
fn week_start() -> i32 {
    // From glibc's <langinfo.h>.
    const NL_TIME_WEEK_1STDAY: c_int = 0x20066;
    const NL_TIME_FIRST_WEEKDAY: c_int = 0x20068;
    unsafe extern "C" {
        fn nl_langinfo(item: c_int) -> *const c_char;
    }
    unsafe {
        // This item is a number in the place of the string pointer.
        let origin = nl_langinfo(NL_TIME_WEEK_1STDAY) as usize as u32;
        let first = nl_langinfo(NL_TIME_FIRST_WEEKDAY);
        if first.is_null() { 0 } else { start_from_locale(origin, *first as u8) }
    }
}

/// `date` at noon, to format its names.
fn at_noon(date: Date) -> Option<glib::DateTime> {
    glib::DateTime::from_local(date.year, date.month, date.day, 12, 0, 0.0).ok()
}

fn format(date: Date, pattern: &str) -> String {
    at_noon(date).and_then(|d| d.format(pattern).ok()).map(|s| s.to_string()).unwrap_or_default()
}

fn today() -> Option<Date> {
    let now = glib::DateTime::now_local().ok()?;
    Some(Date { year: now.year(), month: now.month(), day: now.day_of_month() })
}

fn show_week_numbers() -> bool {
    let schema = "org.gnome.desktop.calendar";
    gio::SettingsSchemaSource::default()
        .and_then(|s| s.lookup(schema, true))
        .is_some_and(|_| gio::Settings::new(schema).boolean("show-weekdate"))
}

/// How wide the calendar is.
pub fn width() -> f32 {
    7.0 * CELL_WIDTH + if show_week_numbers() { WEEK_WIDTH } else { 0.0 } + 2.0 * PADDING
}

/// A label centered in a box at `x`, `y` of `parent`.
unsafe fn centered(
    parent: *mut ClutterActor,
    font: &CStr,
    text: &str,
    color: &ffi::CoglColor,
    (x, y, width, height): (f32, f32, f32, f32),
) {
    unsafe {
        let label = ui::label(font, text, color);
        ffi::clutter_actor_add_child(parent, label);
        ffi::clutter_actor_set_position(
            label,
            (x + (width - ffi::clutter_actor_get_width(label)) / 2.0).floor(),
            (y + (height - ffi::clutter_actor_get_height(label)) / 2.0).floor(),
        );
    }
}

impl Notifications {
    /// Show the month `delta` months away from the one shown.
    pub(super) fn turn_month(self: &Rc<Self>, delta: i32) {
        {
            let mut state = self.state.borrow_mut();
            // A century either way is plenty, and keeps the dates valid.
            state.list.month = state.list.month.saturating_add(delta).clamp(-1200, 1200);
        }
        self.build_list();
    }

    pub(super) fn show_current_month(self: &Rc<Self>) {
        self.state.borrow_mut().list.month = 0;
        self.build_list();
    }

    /// The calendar for the month in view, and its height. Add it to `parent`.
    pub(super) fn add_calendar(self: &Rc<Self>, parent: *mut ClutterActor, x: f32) -> f32 {
        let Some(today) = today() else { return 0.0 };
        let (offset, width) = (self.state.borrow().list.month, width());
        let (year, month) = add_months(today.year, today.month, offset);
        let first = week_start();
        let weeks = show_week_numbers();
        let days = grid(year, month, first);
        let left = PADDING + if weeks { WEEK_WIDTH } else { 0.0 };
        unsafe {
            let calendar = ffi::clutter_actor_new();
            ffi::clutter_actor_set_reactive(calendar, 1);
            ffi::clutter_actor_add_child(parent, calendar);
            ffi::clutter_actor_set_position(calendar, x, 0.0);
            let weak = Rc::downgrade(self);
            gobj(calendar).connect_local("scroll-event", false, move |args| {
                with(&weak, |n| {
                    let rows = ui::scroll_rows(ui::event_arg(args), &mut n.state.borrow_mut().list.month_scroll);
                    if rows != 0 {
                        n.turn_month(rows.signum());
                    }
                });
                // Not on to the list, which scrolls on the same event.
                Some(true.to_value())
            });

            // Today, in full. A click returns to its month.
            let mut y = SPACING;
            let heading = format(today, "%A, %-d. %B %Y");
            let today_row = self.calendar_button(calendar, (SPACING, y, width - 2.0 * SPACING, ROW_HEIGHT), |n| {
                n.show_current_month()
            });
            centered(today_row, DATE_FONT, &heading, &ui::TEXT_COLOR, (0.0, 0.0, width - 2.0 * SPACING, ROW_HEIGHT));
            y += ROW_HEIGHT + SPACING;

            for (text, button_x, delta) in [("‹", PADDING, -1), ("›", width - PADDING - ARROW_WIDTH, 1)] {
                let arrow = self.calendar_button(calendar, (button_x, y, ARROW_WIDTH, ROW_HEIGHT), move |n| {
                    n.turn_month(delta)
                });
                centered(arrow, DATE_FONT, text, &ui::TEXT_COLOR, (0.0, 0.0, ARROW_WIDTH, ROW_HEIGHT));
            }
            let shown = Date { year, month, day: 1 };
            // `%OB` is the month's name as it stands alone.
            centered(calendar, SUMMARY_FONT, &format(shown, "%OB %Y"), &ui::TEXT_COLOR, (0.0, y, width, ROW_HEIGHT));
            y += ROW_HEIGHT + SPACING;

            for (column, day) in days.iter().take(7).enumerate() {
                let cell = (left + column as f32 * CELL_WIDTH, y, CELL_WIDTH, CELL_HEIGHT);
                centered(calendar, ui::BODY_FONT, &format(*day, "%a"), &ui::HINT_COLOR, cell);
            }
            y += CELL_HEIGHT;

            for week in days.chunks(7) {
                if weeks {
                    // The ISO week that holds the row's Thursday.
                    let thursday = week[(3 - first).rem_euclid(7) as usize];
                    let number = at_noon(thursday).map(|d| d.week_of_year().to_string()).unwrap_or_default();
                    centered(calendar, ui::BODY_FONT, &number, &ui::HINT_COLOR, (PADDING, y, WEEK_WIDTH, CELL_HEIGHT));
                }
                for (column, day) in week.iter().enumerate() {
                    let cell_x = left + column as f32 * CELL_WIDTH;
                    if *day == today {
                        let mark = ffi::clutter_actor_new();
                        ffi::clutter_actor_set_background_color(mark, &TODAY_COLOR);
                        ffi::clutter_actor_set_size(mark, CELL_WIDTH - 6.0, CELL_HEIGHT - 4.0);
                        ffi::clutter_actor_set_position(mark, cell_x + 3.0, y + 2.0);
                        ffi::clutter_actor_add_child(calendar, mark);
                    }
                    let color = if day.month == month || *day == today { &ui::TEXT_COLOR } else { &ui::HINT_COLOR };
                    centered(calendar, ui::BODY_FONT, &day.day.to_string(), color, (cell_x, y, CELL_WIDTH, CELL_HEIGHT));
                }
                y += CELL_HEIGHT;
            }

            y += SPACING;
            ffi::clutter_actor_set_size(calendar, width, y);
            y
        }
    }

    /// A button without a color of its own until the pointer is over it.
    fn calendar_button(
        self: &Rc<Self>,
        parent: *mut ClutterActor,
        (x, y, width, height): (f32, f32, f32, f32),
        pressed: impl Fn(&Rc<Self>) + 'static,
    ) -> *mut ClutterActor {
        let button = unsafe {
            let button = ffi::clutter_actor_new();
            ffi::clutter_actor_set_reactive(button, 1);
            ffi::clutter_actor_set_position(button, x, y);
            ffi::clutter_actor_set_size(button, width, height);
            ffi::clutter_actor_add_child(parent, button);
            button
        };
        crate::windows::hover_highlight(button, ui::TRANSPARENT, HOVER_COLOR);
        let weak = Rc::downgrade(self);
        unsafe { gobj(button) }.connect_local("button-press-event", false, move |_| {
            with(&weak, &pressed);
            Some(true.to_value())
        });
        button
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn date(year: i32, month: i32, day: i32) -> Date {
        Date { year, month, day }
    }

    #[test]
    fn knows_weekdays_and_month_lengths() {
        assert_eq!(weekday(date(2026, 11, 27)), 4); // a Friday
        assert_eq!(weekday(date(2024, 1, 1)), 0); // a Monday
        assert_eq!(weekday(date(2000, 2, 29)), 1); // a Tuesday
        assert_eq!(days_in_month(2024, 2), 29);
        assert_eq!(days_in_month(2100, 2), 28);
        assert_eq!(days_in_month(2000, 2), 29);
    }

    #[test]
    fn months_cross_years() {
        assert_eq!(add_months(2026, 12, 1), (2027, 1));
        assert_eq!(add_months(2026, 1, -1), (2025, 12));
        assert_eq!(add_months(2026, 11, -23), (2024, 12));
    }

    #[test]
    fn fills_six_weeks_from_the_first_weekday() {
        let monday_first = grid(2026, 11, 0);
        assert_eq!(monday_first.len(), 42);
        assert_eq!(monday_first[0], date(2026, 10, 26));
        assert_eq!(monday_first[6], date(2026, 11, 1)); // a Sunday
        assert_eq!(monday_first[35], date(2026, 11, 30));
        assert_eq!(monday_first[41], date(2026, 12, 6));
        let sunday_first = grid(2026, 11, 6);
        assert_eq!(sunday_first[0], date(2026, 11, 1));
        for cell in &monday_first {
            assert_eq!(weekday(*cell), (monday_first.iter().position(|d| d == cell).unwrap() % 7) as i32);
        }
    }

    #[test]
    fn reads_the_locale_week() {
        // The values of de_DE and en_US; 1997-11-30 was a Sunday.
        assert_eq!(start_from_locale(19971130, 2), 0);
        assert_eq!(start_from_locale(19971130, 1), 6);
        assert_eq!(start_from_locale(0, 1), 0);
    }
}
