//! EventKit Reminders — read and write `EKReminder` in-process (pearl
//! th-94cc4a, reminders slice).
//!
//! ## Why native, when `calendar` shells out to `ical`
//! `ical` (BRO3886) is calendar-only; there is no equivalent reminders CLI worth
//! taking a dependency on, and `osascript` → Reminders.app trades the EventKit
//! grant for a flakier Automation grant that needs the app running. EventKit is
//! already linked here for the access prompts, so reminders are read and written
//! directly. That also means **no subprocess exists at all** — nothing to escape
//! the kernel sandbox, no shell, no injection surface, the same shape as the
//! `imessage` read half.
//!
//! ## Threading
//! Every call here **blocks** — the EventKit fetch is completion-block based and
//! this waits on it. Callers on an async runtime must use `spawn_blocking`
//! ([`smooth_tools::reminders`] does).
//!
//! This lives in the menu-bar crate because that's the workspace's macOS
//! quarantine: the one crate allowed `unsafe` for objc2 FFI. The TCC grant for
//! all of it is next door in [`crate::eventkit`].

#![cfg(target_os = "macos")]

use std::sync::mpsc;
use std::time::Duration;

use anyhow::{anyhow, Result};
use block2::RcBlock;
use objc2::rc::Retained;
use objc2_event_kit::{EKCalendar, EKEntityType, EKEventStore, EKReminder};
use objc2_foundation::{NSArray, NSDateComponents, NSInteger, NSString};

/// Hard cap on an EventKit fetch. The store can hang indefinitely when the TCC
/// daemon is wedged; a stuck fetch would otherwise stall the whole agent turn.
const FETCH_TIMEOUT: Duration = Duration::from_secs(30);

/// EventKit's "this component isn't set" sentinel (`NSDateComponentUndefined`).
const UNDEFINED: NSInteger = NSInteger::MAX;

/// A reminder's due date. `time` is `None` for a date-only ("all day") due date,
/// which is what Reminders shows as a day with no time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Due {
    pub year: i32,
    pub month: u32,
    pub day: u32,
    pub time: Option<(u32, u32)>,
}

/// One reminder, flattened out of EventKit so no objc2 type escapes this crate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reminder {
    /// `calendarItemIdentifier` — the handle `complete` takes.
    pub id: String,
    pub title: String,
    /// The reminder list it belongs to (an `EKCalendar` title).
    pub list: String,
    pub completed: bool,
    pub due: Option<Due>,
    pub notes: Option<String>,
}

/// The names of the user's reminder lists.
///
/// # Errors
/// Never fails today; returns `Result` so a future EventKit error path doesn't
/// change the signature.
pub fn lists() -> Result<Vec<String>> {
    // SAFETY: plain designated initializer, no arguments to get wrong.
    let store = unsafe { EKEventStore::new() };
    Ok(reminder_lists(&store))
}

/// Every reminder, newest EventKit ordering, optionally including completed
/// ones and optionally filtered to one list (case-insensitive).
///
/// # Errors
/// Fails when the EventKit fetch doesn't call back within [`FETCH_TIMEOUT`].
pub fn list(include_completed: bool, list_name: Option<&str>) -> Result<Vec<Reminder>> {
    // SAFETY: plain designated initializer.
    let store = unsafe { EKEventStore::new() };
    let mut found = fetch(&store, include_completed)?;
    if let Some(want) = list_name {
        found.retain(|r| r.list.eq_ignore_ascii_case(want));
    }
    Ok(found)
}

/// Create a reminder. `list_name` picks the reminder list (default list when
/// `None`); an unknown name is an error naming the valid ones.
///
/// # Errors
/// Fails when the named list doesn't exist, when there's no default reminder
/// list to fall back on, or when EventKit refuses the save.
pub fn add(title: &str, due: Option<Due>, list_name: Option<&str>) -> Result<Reminder> {
    // SAFETY: plain designated initializer.
    let store = unsafe { EKEventStore::new() };
    let calendar = pick_calendar(&store, list_name)?;

    // SAFETY: a class constructor taking the live store we just made.
    let reminder = unsafe { EKReminder::reminderWithEventStore(&store) };
    // SAFETY: property setters on a live reminder; the NSStrings outlive the
    // calls (EventKit copies them).
    unsafe {
        reminder.setTitle(Some(&NSString::from_str(title)));
        reminder.setCalendar(Some(&calendar));
        if let Some(due) = due {
            reminder.setDueDateComponents(Some(&components(due)));
        }
    }
    // SAFETY: saving a reminder built against this same store.
    unsafe { store.saveReminder_commit_error(&reminder, true) }.map_err(ns_err("saving the reminder"))?;
    Ok(read(&reminder))
}

/// Mark the reminder with `id` (a `calendarItemIdentifier`) completed.
///
/// # Errors
/// Fails when no open reminder has that id, or when EventKit refuses the save.
pub fn complete(id: &str) -> Result<Reminder> {
    // SAFETY: plain designated initializer.
    let store = unsafe { EKEventStore::new() };
    // ponytail: find it by scanning the fetch rather than
    // `calendarItemWithIdentifier` + a downcast — reminder counts are small, and
    // this reuses the one fetch path that's already tested.
    let target = fetch_raw(&store, true)?
        .into_iter()
        .find(|r| {
            // SAFETY: reading an identifier off a live reminder.
            unsafe { r.calendarItemIdentifier() }.to_string() == id
        })
        .ok_or_else(|| anyhow!("no reminder with id `{id}` — run a list first and use the `id` from it"))?;

    // SAFETY: setting a property on a live reminder from this store.
    unsafe { target.setCompleted(true) };
    // SAFETY: saving a reminder that came out of this same store.
    unsafe { store.saveReminder_commit_error(&target, true) }.map_err(ns_err("completing the reminder"))?;
    Ok(read(&target))
}

/// The reminder list `add` should write to: the named one, or EventKit's
/// default. An unknown name lists the valid ones rather than silently landing
/// the reminder somewhere else.
fn pick_calendar(store: &EKEventStore, list_name: Option<&str>) -> Result<Retained<EKCalendar>> {
    let Some(want) = list_name else {
        // SAFETY: a property read on a live store.
        return unsafe { store.defaultCalendarForNewReminders() }
            .ok_or_else(|| anyhow!("macOS has no default reminder list — open Reminders.app and create one"));
    };
    // SAFETY: a property read on a live store; the returned array is owned.
    let calendars = unsafe { store.calendarsForEntityType(EKEntityType::Reminder) };
    for i in 0..calendars.count() {
        let cal = calendars.objectAtIndex(i);
        // SAFETY: a property read on a live calendar.
        if unsafe { cal.title() }.to_string().eq_ignore_ascii_case(want) {
            return Ok(cal);
        }
    }
    Err(anyhow!("no reminder list named `{want}`. Lists: {}", reminder_lists(store).join(", ")))
}

/// Titles of every reminder list, in EventKit order.
fn reminder_lists(store: &EKEventStore) -> Vec<String> {
    // SAFETY: a property read on a live store.
    let calendars = unsafe { store.calendarsForEntityType(EKEntityType::Reminder) };
    (0..calendars.count())
        .map(|i| {
            // SAFETY: a property read on a live calendar.
            unsafe { calendars.objectAtIndex(i).title() }.to_string()
        })
        .collect()
}

/// Fetch and flatten. See [`fetch_raw`] for the blocking dance.
fn fetch(store: &EKEventStore, include_completed: bool) -> Result<Vec<Reminder>> {
    Ok(fetch_raw(store, include_completed)?.iter().map(|r| read(r)).collect())
}

/// Run one `fetchRemindersMatchingPredicate:` and block for the result.
///
/// `include_completed` picks the predicate: all reminders in every list, or only
/// the incomplete ones (`None`/`None` bounds = "with any due date, or none").
fn fetch_raw(store: &EKEventStore, include_completed: bool) -> Result<Vec<Retained<EKReminder>>> {
    // SAFETY: predicate constructors on a live store; passing `None` for the
    // calendars means "every list", and for the dates "unbounded".
    let predicate = unsafe {
        if include_completed {
            store.predicateForRemindersInCalendars(None)
        } else {
            store.predicateForIncompleteRemindersWithDueDateStarting_ending_calendars(None, None, None)
        }
    };

    let (tx, rx) = mpsc::channel();
    let completion = RcBlock::new(move |found: *mut NSArray<EKReminder>| {
        // The callback fires on an EventKit-owned queue. Copy what we need out
        // of the array here — nothing objc2 crosses the channel.
        let mut out = Vec::new();
        if !found.is_null() {
            // SAFETY: EventKit hands us a live, non-null array for the duration
            // of the callback.
            let array = unsafe { &*found };
            for i in 0..array.count() {
                out.push(array.objectAtIndex(i));
            }
        }
        let _ = tx.send(out);
    });
    // SAFETY: the block outlives the call below, and EventKit copies it for its
    // async callback.
    let _handle = unsafe { store.fetchRemindersMatchingPredicate_completion(&predicate, &completion) };

    match rx.recv_timeout(FETCH_TIMEOUT) {
        Ok(found) => Ok(found),
        Err(_) => {
            // Same reasoning as the access prompt: the callback may still fire
            // after we give up, so leak the block rather than risk a
            // use-after-free.
            std::mem::forget(completion);
            Err(anyhow!(
                "EventKit did not answer within {}s — Reminders access may be waiting on a permission prompt",
                FETCH_TIMEOUT.as_secs()
            ))
        }
    }
}

/// Flatten one live `EKReminder` into the plain [`Reminder`] callers see.
fn read(r: &EKReminder) -> Reminder {
    // SAFETY: property reads on a live reminder.
    unsafe {
        Reminder {
            id: r.calendarItemIdentifier().to_string(),
            title: r.title().to_string(),
            list: r.calendar().map_or_else(|| "(no list)".to_owned(), |c| c.title().to_string()),
            completed: r.isCompleted(),
            due: r.dueDateComponents().and_then(|c| due_from(&c)),
            notes: r.notes().map(|n| n.to_string()).filter(|n| !n.is_empty()),
        }
    }
}

/// `NSDateComponents` → [`Due`]. `None` when the date half isn't set, which is
/// how a reminder with no due date presents.
fn due_from(c: &NSDateComponents) -> Option<Due> {
    let (year, month, day, hour, minute) = (c.year(), c.month(), c.day(), c.hour(), c.minute());
    if year == UNDEFINED || month == UNDEFINED || day == UNDEFINED {
        return None;
    }
    let time = if hour == UNDEFINED || minute == UNDEFINED {
        None
    } else {
        Some((u32::try_from(hour).ok()?, u32::try_from(minute).ok()?))
    };
    Some(Due {
        year: i32::try_from(year).ok()?,
        month: u32::try_from(month).ok()?,
        day: u32::try_from(day).ok()?,
        time,
    })
}

/// [`Due`] → `NSDateComponents`. A date-only [`Due`] leaves hour/minute unset,
/// which is what makes Reminders treat it as an all-day due date.
fn components(due: Due) -> Retained<NSDateComponents> {
    // A value too large for NSInteger can't be a real date component, so it
    // falls back to "unset" rather than wrapping into a wrong date.
    let n = |v: i64| NSInteger::try_from(v).unwrap_or(UNDEFINED);
    {
        let c = NSDateComponents::new();
        c.setYear(n(i64::from(due.year)));
        c.setMonth(n(i64::from(due.month)));
        c.setDay(n(i64::from(due.day)));
        if let Some((h, m)) = due.time {
            c.setHour(n(i64::from(h)));
            c.setMinute(n(i64::from(m)));
        }
        c
    }
}

/// Turn an `NSError` from a save into an `anyhow::Error` that says what failed.
fn ns_err(what: &'static str) -> impl FnOnce(Retained<objc2_foundation::NSError>) -> anyhow::Error {
    move |e| anyhow!("{what} failed: {}", e.localizedDescription())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The pure conversions are the only part testable without a TCC grant —
    /// and they're where the off-by-one bugs live.
    #[test]
    fn undefined_components_read_back_as_no_due_date() {
        let c = NSDateComponents::new();
        assert_eq!(due_from(&c), None, "an empty components object is not a due date");
    }

    #[test]
    fn a_due_date_round_trips_through_eventkit_components() {
        let with_time = Due {
            year: 2026,
            month: 8,
            day: 3,
            time: Some((14, 30)),
        };
        assert_eq!(due_from(&components(with_time)), Some(with_time));

        // Date-only must stay date-only — setting hour/minute here would turn
        // every "due Tuesday" into "due Tuesday at midnight".
        let all_day = Due {
            year: 2026,
            month: 12,
            day: 25,
            time: None,
        };
        assert_eq!(due_from(&components(all_day)), Some(all_day));
    }

    #[test]
    fn a_date_only_due_leaves_the_time_components_unset() {
        let c = components(Due {
            year: 2026,
            month: 1,
            day: 2,
            time: None,
        });
        assert_eq!(c.hour(), UNDEFINED);
        assert_eq!(c.minute(), UNDEFINED);
    }

    #[test]
    fn store_construction_and_list_enumeration_never_panic() {
        // Machine-dependent result (no grant in a test binary → empty), but the
        // FFI calls must be well-formed and total.
        let _ = lists();
    }
}
