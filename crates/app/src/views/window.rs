//! The main window's content: the account it shows (each has its own
//! `RootView`, kept while another is shown, so its open chat stays), and a
//! call with another of the app's accounts on top.

use std::collections::HashMap;

use gpui_kit::prelude::*;
use gpui_kit::{Context, Entity, EntityId, Subscription, Window, div};

use crate::accounts::Accounts;
use crate::views::root::RootView;

pub struct WindowView {
    accounts: Entity<Accounts>,
    /// Each account's, by its state's entity.
    roots: HashMap<EntityId, Entity<RootView>>,
    /// The account shown when last drawn.
    shown: Option<EntityId>,
    /// Another account's call (or lobby) is over the one shown.
    other_call: bool,
    _subscription: Subscription,
}

impl WindowView {
    pub fn new(accounts: Entity<Accounts>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let subscription = cx.observe_in(&accounts, window, |this, _, window, cx| {
            this.sync(window, cx);
            cx.notify();
        });
        let mut this = Self { accounts, roots: HashMap::new(), shown: None, other_call: false, _subscription: subscription };
        this.sync(window, cx);
        // Accounts that couldn't be opened, said once the window is up.
        let errors = this.accounts.update(cx, |accounts, _| std::mem::take(&mut accounts.startup_errors));
        if !errors.is_empty() {
            cx.defer_in(window, move |_, window, cx| {
                use gpui_kit::component::WindowExt as _;
                for error in errors {
                    window.push_notification(gpui_kit::component::notification::Notification::error(error), cx);
                }
            });
        }
        this
    }

    /// The keyboard goes to the account shown.
    fn focus_shown(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(root) = self.shown.and_then(|id| self.roots.get(&id)) {
            root.update(cx, |root, cx| root.focus(window, cx));
        }
    }

    /// A view for each account (and none for one that's gone); the one
    /// coming into view takes the keyboard.
    fn sync(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (states, active) = {
            let accounts = self.accounts.read(cx);
            let states: Vec<_> = accounts.list.iter().map(|account| account.state.clone()).collect();
            let active = accounts.list.get(accounts.active).map(|account| account.state.entity_id());
            (states, active)
        };
        self.roots.retain(|id, _| states.iter().any(|state| state.entity_id() == *id));
        for state in states {
            self.roots.entry(state.entity_id()).or_insert_with(|| cx.new(|cx| RootView::new(state, window, cx)));
        }
        if active != self.shown {
            // A dialog (Settings, a chat's details) is the last account's.
            if self.shown.is_some() {
                use gpui_kit::component::WindowExt as _;
                window.close_all_dialogs(cx);
            }
            self.shown = active;
            // The account now shown is in front if the window is.
            let window_active = window.is_window_active();
            for state in self.accounts.read(cx).list.iter().map(|account| account.state.clone()).collect::<Vec<_>>() {
                state.update(cx, |state, _| state.window_active = window_active && state.shown);
            }
            if let Some(root) = active.and_then(|id| self.roots.get(&id)) {
                root.update(cx, |root, cx| root.focus(window, cx));
            }
        }
        // Another account's call ended: the keyboard was on it.
        let other_call = self.roots.iter().filter(|(id, _)| Some(**id) != self.shown).any(|(_, root)| root.read(cx).call_view(cx).is_some());
        if std::mem::replace(&mut self.other_call, other_call) && !other_call {
            self.focus_shown(window, cx);
        }
    }
}

impl Render for WindowView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let root = self.shown.and_then(|id| self.roots.get(&id)).cloned();
        // A call (or a ring) with another account shows over this one.
        let other_call = self
            .roots
            .iter()
            .filter(|(id, _)| Some(**id) != self.shown)
            .find_map(|(_, root)| root.read(cx).call_view(cx));
        div()
            .relative()
            .size_full()
            .children(root)
            .children(other_call)
    }
}

#[cfg(test)]
mod tests {
    use gpui_kit::{TestAppContext, VisualTestContext, WindowBounds, WindowOptions, px, size};
    use signal_backend::{Backend, CallLines, Config};

    use super::*;
    use crate::views::test_support::{click_at, wait};

    #[gpui_kit::test]
    fn the_window_shows_one_account_and_switches_to_another(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        cx.executor().allow_parking();
        let root = tempfile::tempdir().unwrap();
        let lines = CallLines::default();
        let started: Vec<_> = (0..2)
            .map(|i| {
                let data_dir = root.path().join(format!("account-{i}"));
                let config = Config { data_dir, db_key: [7; 32], device_name: "test".into(), demo: true, former_data_dirs: Vec::new(), call_lines: lines.clone() };
                Backend::start(config).unwrap()
            })
            .collect();
        let backends: Vec<Backend> = started.iter().map(|(backend, _)| backend.clone()).collect();
        let accounts = cx.update(|cx| Accounts::for_test(root.path().to_owned(), started, cx));
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(gpui_kit::Bounds { origin: Default::default(), size: size(px(1100.), px(760.)) })),
            ..Default::default()
        };
        let (window, view) = cx
            .update(|cx| {
                let accounts = accounts.clone();
                gpui_kit::open_window(options, cx, |window, cx| cx.new(|cx| WindowView::new(accounts, window, cx)))
            })
            .unwrap();
        let mut vcx = VisualTestContext::from_window(window, cx);
        let vcx = &mut vcx;
        let states: Vec<_> = accounts.read_with(vcx, |accounts, _| accounts.list.iter().map(|a| a.state.clone()).collect());
        wait(vcx, "both accounts", &mut |cx| states.iter().all(|s| s.read_with(cx, |s, _| s.account.is_some())));

        // The first is shown; the other isn't in front, so its chats
        // aren't read and its messages notify.
        let shown = |cx: &mut VisualTestContext| states.iter().map(|s| s.read_with(cx, |s, _| (s.shown, s.window_active))).collect::<Vec<_>>();
        assert!(shown(vcx)[0].0);
        assert_eq!(shown(vcx)[1], (false, false));

        let root_of = |cx: &mut VisualTestContext, i: usize| view.read_with(cx, |view, _| view.roots[&states[i].entity_id()].clone());
        // A hidden account's error waits for it rather than showing over
        // this one.
        states[1].update(vcx, |_, cx| cx.emit(crate::state::AppEvent::Error("from the other account".into())));
        vcx.run_until_parked();
        assert_eq!(root_of(vcx, 1).read_with(vcx, |root, _| root.held.len()), 1);
        // An account linked before isn't one to cancel, even on the link
        // screen (its registration didn't load, say).
        let linked = states[1].update(vcx, |state, _| state.account.take());
        assert!(!accounts.read_with(vcx, |accounts, cx| accounts.can_cancel(1, cx)));
        states[1].update(vcx, |state, _| state.account = linked);

        // The switcher lists both; picking the second shows it.
        click_at(vcx, "nav-account-1");
        wait(vcx, "the second account", &mut |cx| accounts.read_with(cx, |a, _| a.active == 1));
        assert!(shown(vcx)[1].0);
        assert_eq!(shown(vcx)[0], (false, false));
        assert!(vcx.update(|_, cx| crate::accounts::active_state(cx)) == Some(states[1].clone()));
        // What waited is shown now.
        assert!(root_of(vcx, 1).read_with(vcx, |root, _| root.held.is_empty()));
        // Its view kept, not made again, on the way back.
        let first_root = root_of(vcx, 0);
        click_at(vcx, "nav-account-0");
        wait(vcx, "the first account", &mut |cx| accounts.read_with(cx, |a, _| a.active == 0));
        assert_eq!(root_of(vcx, 0), first_root);
        for backend in backends {
            backend.shutdown();
        }
    }
}
