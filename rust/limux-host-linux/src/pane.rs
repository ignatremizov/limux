//! PaneWidget: a tabbed container with action icons in the tab bar.
//!
//! Layout: [tab1 x] [tab2 x] ... ←spacer→ [terminal] [browser] [split-h] [split-v] [close]
//!
//! All on one line. Tabs left-justified, icons right-justified.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::atomic::{AtomicU32, Ordering};

use gtk::glib;
use gtk::prelude::*;
use gtk4 as gtk;

use crate::layout_state::{PaneState, TabContentState, TabState as SavedTabState};
use crate::terminal::{self, TerminalCallbacks};

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

type PaneSplitCallback = dyn Fn(&gtk::Widget, gtk::Orientation);
type PaneWidgetCallback = dyn Fn(&gtk::Widget);
type PaneSignalCallback = dyn Fn();
type PanePathCallback = dyn Fn(&str);
type PaneDesktopNotificationCallback = dyn Fn(&str, &str);
type PaneSplitWithTabCallback = dyn Fn(&gtk::Widget, &gtk::Widget, gtk::Orientation, String, bool);

pub struct PaneCallbacks {
    pub on_split: Box<PaneSplitCallback>,
    pub on_close_pane: Box<PaneWidgetCallback>,
    pub on_bell: Box<PaneSignalCallback>,
    pub on_desktop_notification: Box<PaneDesktopNotificationCallback>,
    pub on_pwd_changed: Box<PanePathCallback>,
    pub on_empty: Box<PaneWidgetCallback>,
    pub on_state_changed: Box<PaneSignalCallback>,
    pub on_split_with_tab: Box<PaneSplitWithTabCallback>,
}

fn next_pane_id() -> u32 {
    static COUNTER: AtomicU32 = AtomicU32::new(1);
    COUNTER.fetch_add(1, Ordering::Relaxed)
}

type TabDragCallback = dyn Fn(bool);

thread_local! {
    static TAB_DRAGGING: Cell<bool> = const { Cell::new(false) };
    static TAB_DRAG_LISTENERS: RefCell<std::collections::HashMap<usize, Box<TabDragCallback>>> =
        RefCell::new(std::collections::HashMap::new());
    static TAB_DRAG_NEXT_ID: Cell<usize> = const { Cell::new(1) };
    static PANE_REGISTRY: RefCell<std::collections::HashMap<u32, std::rc::Weak<PaneInternals>>> =
        RefCell::new(std::collections::HashMap::new());
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct TabDragPayload {
    pane_id: u32,
    tab_id: String,
}

impl TabDragPayload {
    fn new(pane_id: u32, tab_id: impl Into<String>) -> Self {
        Self {
            pane_id,
            tab_id: tab_id.into(),
        }
    }

    fn encode(&self) -> String {
        format!("{}:{}", self.pane_id, self.tab_id)
    }

    fn decode(raw: &str) -> Option<Self> {
        let (pane_id, tab_id) = raw.split_once(':')?;
        if tab_id.is_empty() {
            return None;
        }
        Some(Self::new(pane_id.parse::<u32>().ok()?, tab_id))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ContentDropZone {
    Center,
    Left,
    Right,
    Top,
    Bottom,
}

pub fn is_tab_dragging() -> bool {
    TAB_DRAGGING.with(|value| value.get())
}

pub fn on_tab_drag_change(callback: impl Fn(bool) + 'static) -> usize {
    TAB_DRAG_LISTENERS.with(|listeners| {
        let id = TAB_DRAG_NEXT_ID.with(|next| {
            let id = next.get();
            next.set(id + 1);
            id
        });
        listeners.borrow_mut().insert(id, Box::new(callback));
        id
    })
}

pub fn remove_tab_drag_listener(id: usize) {
    TAB_DRAG_LISTENERS.with(|listeners| {
        listeners.borrow_mut().remove(&id);
    });
}

pub fn set_workspace_dragging_all(active: bool) {
    PANE_REGISTRY.with(|registry| {
        for weak in registry.borrow().values() {
            if let Some(internals) = weak.upgrade() {
                internals.workspace_dragging.set(active);
            }
        }
    });
}

fn set_tab_dragging(active: bool) {
    TAB_DRAGGING.with(|value| value.set(active));
    TAB_DRAG_LISTENERS.with(|listeners| {
        for callback in listeners.borrow().values() {
            callback(active);
        }
    });
}

fn register_pane(id: u32, internals: &Rc<PaneInternals>) {
    PANE_REGISTRY.with(|registry| {
        registry.borrow_mut().insert(id, Rc::downgrade(internals));
    });
}

fn unregister_pane(id: u32) {
    PANE_REGISTRY.with(|registry| {
        registry.borrow_mut().remove(&id);
    });
}

fn lookup_pane_internals(id: u32) -> Option<Rc<PaneInternals>> {
    PANE_REGISTRY.with(|registry| registry.borrow().get(&id)?.upgrade())
}

pub fn find_pane_widget_by_id(pane_id: u32) -> Option<gtk::Widget> {
    lookup_pane_internals(pane_id).map(|internals| internals.pane_outer.clone().upcast())
}

#[derive(Clone)]
struct TabContextMenuContext {
    tab_strip: gtk::Box,
    content_stack: gtk::Stack,
    tab_state: Rc<RefCell<TabState>>,
    callbacks: Rc<PaneCallbacks>,
    pane_outer: gtk::Box,
    label: gtk::Label,
    pin_icon: gtk::Label,
}

// ---------------------------------------------------------------------------
// CSS
// ---------------------------------------------------------------------------

pub const PANE_CSS: &str = r#"
.limux-pane-header {
    background-color: rgba(30, 30, 30, 1);
    border-bottom: 1px solid rgba(255, 255, 255, 0.06);
    min-height: 30px;
    padding: 0 2px;
}
.limux-tab {
    background: none;
    border: none;
    border-radius: 4px 4px 0 0;
    padding: 4px 4px 4px 10px;
    color: rgba(255, 255, 255, 0.45);
    min-height: 0;
    font-size: 12px;
}
.limux-tab:hover {
    color: rgba(255, 255, 255, 0.7);
    background: rgba(255, 255, 255, 0.04);
}
.limux-tab-active {
    color: white;
    background: rgba(255, 255, 255, 0.08);
}
.limux-tab-close {
    background: none;
    border: none;
    border-radius: 3px;
    padding: 1px;
    min-height: 0;
    min-width: 0;
    color: rgba(255, 255, 255, 0.25);
    margin-left: 4px;
}
.limux-tab-close:hover {
    color: rgba(255, 255, 255, 0.8);
    background: rgba(255, 255, 255, 0.1);
}
.limux-pane-action {
    background: none;
    border: none;
    border-radius: 4px;
    padding: 4px 5px;
    min-height: 0;
    min-width: 0;
    color: rgba(255, 255, 255, 0.35);
}
.limux-pane-action:hover {
    background: rgba(255, 255, 255, 0.08);
    color: rgba(255, 255, 255, 0.8);
}
.limux-split-icon {
    border: 1px solid rgba(255, 255, 255, 0.4);
    border-radius: 2px;
    min-width: 16px;
    min-height: 12px;
    padding: 0;
}
.limux-split-icon:hover {
    border-color: rgba(255, 255, 255, 0.8);
}
.limux-split-half-v {
    min-width: 6px;
    min-height: 10px;
}
.limux-split-half-h {
    min-width: 14px;
    min-height: 4px;
}
.limux-split-btn {
    background: none;
    border: none;
    border-radius: 4px;
    padding: 4px 5px;
    min-height: 0;
    min-width: 0;
}
.limux-split-btn:hover {
    background: rgba(255, 255, 255, 0.08);
}
.limux-pin-icon {
    font-size: 9px;
    margin-right: 2px;
}
.limux-tab-rename-entry {
    background: rgba(255, 255, 255, 0.1);
    color: white;
    border: 1px solid rgba(0, 145, 255, 0.5);
    border-radius: 3px;
    padding: 1px 4px;
    min-height: 0;
    font-size: 12px;
}
.limux-drop-preview {
    background: rgba(0, 145, 255, 0.24);
    border: 1px solid rgba(0, 145, 255, 0.65);
    border-radius: 10px;
}
.limux-drop-preview-center {
    background: rgba(0, 145, 255, 0.14);
}
"#;

// ---------------------------------------------------------------------------
// PaneWidget builder
// ---------------------------------------------------------------------------

pub fn create_pane(
    callbacks: Rc<PaneCallbacks>,
    working_directory: Option<&str>,
    initial_state: Option<&PaneState>,
    skip_default_tab: bool,
) -> gtk::Box {
    let ws_wd: Rc<RefCell<Option<String>>> =
        Rc::new(RefCell::new(working_directory.map(|s| s.to_string())));
    let pane_id = next_pane_id();
    let workspace_dragging = Rc::new(Cell::new(false));

    let outer = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .hexpand(true)
        .vexpand(true)
        .build();
    unsafe {
        outer.set_data("limux-pane-id", pane_id);
    }

    // The single header line: tabs (left) + action icons (right)
    let header = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(0)
        .build();
    header.add_css_class("limux-pane-header");

    let tab_strip = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(0)
        .hexpand(true)
        .build();

    let content_stack = gtk::Stack::new();
    content_stack.set_transition_type(gtk::StackTransitionType::None);
    content_stack.set_hexpand(true);
    content_stack.set_vexpand(true);

    let content_overlay = gtk::Overlay::new();
    content_overlay.set_hexpand(true);
    content_overlay.set_vexpand(true);
    content_overlay.set_child(Some(&content_stack));

    let content_drop_overlay = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    content_drop_overlay.set_halign(gtk::Align::Start);
    content_drop_overlay.set_valign(gtk::Align::Start);
    content_drop_overlay.set_visible(false);
    content_drop_overlay.set_can_target(false);
    content_overlay.add_overlay(&content_drop_overlay);

    let actions = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(1)
        .build();

    let new_term_btn = icon_button("utilities-terminal-symbolic", "New terminal tab");
    let new_browser_btn = icon_button("limux-globe-symbolic", "New browser tab");
    let split_h_btn = icon_button("limux-split-horizontal-symbolic", "Split right");
    let split_v_btn = icon_button("limux-split-vertical-symbolic", "Split down");
    let close_btn = icon_button("window-close-symbolic", "Close pane");

    actions.append(&new_term_btn);
    actions.append(&new_browser_btn);
    actions.append(&split_h_btn);
    actions.append(&split_v_btn);
    actions.append(&close_btn);

    header.append(&tab_strip);
    header.append(&actions);

    outer.append(&header);
    outer.append(&content_overlay);

    // Shared state for tabs
    let tab_state = Rc::new(std::cell::RefCell::new(TabState {
        tabs: Vec::new(),
        active_tab: None,
    }));

    if let Some(saved_state) = initial_state {
        restore_tabs_from_state(
            &tab_strip,
            &content_stack,
            &tab_state,
            &callbacks,
            working_directory,
            &outer,
            saved_state,
        );
    } else if !skip_default_tab {
        add_terminal_tab_inner(
            &tab_strip,
            &content_stack,
            &tab_state,
            &callbacks,
            working_directory,
            &outer,
            None,
        );
    }

    // Wire action buttons
    {
        let ts = tab_strip.clone();
        let cs = content_stack.clone();
        let state = tab_state.clone();
        let cb = callbacks.clone();
        let ow = outer.clone();
        let wd = ws_wd.clone();
        new_term_btn.connect_clicked(move |_| {
            let dir = wd.borrow().clone();
            add_terminal_tab_inner(&ts, &cs, &state, &cb, dir.as_deref(), &ow, None);
        });
    }
    {
        let ts = tab_strip.clone();
        let cs = content_stack.clone();
        let state = tab_state.clone();
        let cb = callbacks.clone();
        let ow = outer.clone();
        new_browser_btn.connect_clicked(move |_| {
            add_browser_tab_inner(&ts, &cs, &state, &cb, &ow, None);
        });
    }
    {
        let pw = outer.clone();
        let cb = callbacks.clone();
        split_h_btn.connect_clicked(move |_| {
            (cb.on_split)(&pw.clone().upcast(), gtk::Orientation::Horizontal);
        });
    }
    {
        let pw = outer.clone();
        let cb = callbacks.clone();
        split_v_btn.connect_clicked(move |_| {
            (cb.on_split)(&pw.clone().upcast(), gtk::Orientation::Vertical);
        });
    }
    {
        let pw = outer.clone();
        let cb = callbacks.clone();
        close_btn.connect_clicked(move |_| {
            (cb.on_close_pane)(&pw.clone().upcast());
        });
    }

    let internals = Rc::new(PaneInternals {
        pane_id,
        tab_state: tab_state.clone(),
        tab_strip: tab_strip.clone(),
        content_stack: content_stack.clone(),
        content_drop_overlay: content_drop_overlay.clone(),
        pane_outer: outer.clone(),
        callbacks: callbacks.clone(),
        working_directory: ws_wd.clone(),
        workspace_dragging: workspace_dragging.clone(),
    });
    register_pane(pane_id, &internals);
    unsafe {
        outer.set_data("limux-pane-internals", internals.clone());
    }
    install_tab_strip_drop_target(&tab_strip, &internals);
    install_content_drop_target(&content_overlay, &internals);
    outer.connect_destroy(move |_| {
        unregister_pane(pane_id);
    });

    outer
}

/// Cycle tabs in the focused pane. `delta`: 1 = next, -1 = prev.
pub fn cycle_tab_in_pane(pane_widget: &gtk::Widget, delta: i32) {
    let outer = pane_widget.downcast_ref::<gtk::Box>();
    let outer = match outer {
        Some(o) => o,
        None => return,
    };
    let internals: Rc<PaneInternals> = unsafe {
        match outer.data::<Rc<PaneInternals>>("limux-pane-internals") {
            Some(ptr) => ptr.as_ref().clone(),
            None => return,
        }
    };

    let ts = internals.tab_state.borrow();
    let len = ts.tabs.len();
    if len <= 1 {
        return;
    }

    let active_idx = ts
        .active_tab
        .as_ref()
        .and_then(|id| ts.tabs.iter().position(|e| e.id == *id))
        .unwrap_or(0);

    let new_idx = (active_idx as i32 + delta).rem_euclid(len as i32) as usize;
    let new_id = ts.tabs[new_idx].id.clone();
    drop(ts);

    activate_tab(
        &internals.tab_strip,
        &internals.content_stack,
        &internals.tab_state,
        &new_id,
    );
    (internals.callbacks.on_state_changed)();
}

// ---------------------------------------------------------------------------
// Internal tab state
// ---------------------------------------------------------------------------

#[derive(Clone)]
enum TabKind {
    Terminal { cwd: Rc<RefCell<Option<String>>> },
    Browser { uri: Rc<RefCell<Option<String>>> },
}

struct TabEntry {
    id: String,
    tab_button: gtk::Box,
    #[allow(dead_code)]
    title_label: gtk::Label,
    content: gtk::Widget,
    custom_name: Option<String>,
    pinned: bool,
    kind: TabKind,
}

struct TabState {
    tabs: Vec<TabEntry>,
    active_tab: Option<String>,
}

/// Shared internals stored on the pane outer Box for external access.
pub struct PaneInternals {
    pane_id: u32,
    tab_state: Rc<std::cell::RefCell<TabState>>,
    tab_strip: gtk::Box,
    content_stack: gtk::Stack,
    content_drop_overlay: gtk::Box,
    pane_outer: gtk::Box,
    callbacks: Rc<PaneCallbacks>,
    working_directory: Rc<std::cell::RefCell<Option<String>>>,
    workspace_dragging: Rc<Cell<bool>>,
}

impl TabState {
    fn find_tab_mut(&mut self, id: &str) -> Option<&mut TabEntry> {
        self.tabs.iter_mut().find(|e| e.id == id)
    }
}

fn next_tab_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

// ---------------------------------------------------------------------------
// Icon button helper
// ---------------------------------------------------------------------------

fn icon_button(icon_name: &str, tooltip: &str) -> gtk::Button {
    let btn = gtk::Button::builder()
        .icon_name(icon_name)
        .tooltip_text(tooltip)
        .has_frame(false)
        .build();
    btn.add_css_class("limux-pane-action");
    btn
}

/// Create a split-pane icon button with two rectangles separated by a divider.
/// Horizontal = left|right panes, Vertical = top/bottom panes.
#[allow(dead_code)]
fn split_icon_button(orientation: gtk::Orientation, tooltip: &str) -> gtk::Button {
    let icon = gtk::Box::new(orientation, 1);
    icon.add_css_class("limux-split-icon");

    let (class_name, count) = match orientation {
        gtk::Orientation::Horizontal => ("limux-split-half-v", 2),
        _ => ("limux-split-half-h", 2),
    };

    for _ in 0..count {
        let half = gtk::Box::new(gtk::Orientation::Vertical, 0);
        half.add_css_class(class_name);
        icon.append(&half);
    }

    let btn = gtk::Button::builder()
        .child(&icon)
        .tooltip_text(tooltip)
        .has_frame(false)
        .build();
    btn.add_css_class("limux-split-btn");
    btn
}

// ---------------------------------------------------------------------------
// Tab creation
// ---------------------------------------------------------------------------

struct TerminalTabOptions<'a> {
    id: Option<&'a str>,
    custom_name: Option<&'a str>,
    pinned: bool,
    cwd: Option<&'a str>,
}

struct BrowserTabOptions<'a> {
    id: Option<&'a str>,
    custom_name: Option<&'a str>,
    pinned: bool,
    uri: Option<&'a str>,
}

fn restore_tabs_from_state(
    tab_strip: &gtk::Box,
    content_stack: &gtk::Stack,
    tab_state: &Rc<RefCell<TabState>>,
    callbacks: &Rc<PaneCallbacks>,
    working_directory: Option<&str>,
    pane_outer: &gtk::Box,
    saved_state: &PaneState,
) {
    if saved_state.tabs.is_empty() {
        add_terminal_tab_inner(
            tab_strip,
            content_stack,
            tab_state,
            callbacks,
            working_directory,
            pane_outer,
            None,
        );
        return;
    }

    for saved_tab in &saved_state.tabs {
        match &saved_tab.content {
            TabContentState::Terminal { cwd } => add_terminal_tab_inner(
                tab_strip,
                content_stack,
                tab_state,
                callbacks,
                cwd.as_deref().or(working_directory),
                pane_outer,
                Some(TerminalTabOptions {
                    id: Some(saved_tab.id.as_str()),
                    custom_name: saved_tab.custom_name.as_deref(),
                    pinned: saved_tab.pinned,
                    cwd: cwd.as_deref().or(working_directory),
                }),
            ),
            TabContentState::Browser { uri } => add_browser_tab_inner(
                tab_strip,
                content_stack,
                tab_state,
                callbacks,
                pane_outer,
                Some(BrowserTabOptions {
                    id: Some(saved_tab.id.as_str()),
                    custom_name: saved_tab.custom_name.as_deref(),
                    pinned: saved_tab.pinned,
                    uri: uri.as_deref(),
                }),
            ),
        }
    }

    let active_tab_id = saved_state
        .active_tab_id
        .as_deref()
        .filter(|candidate| {
            tab_state
                .borrow()
                .tabs
                .iter()
                .any(|tab| tab.id == *candidate)
        })
        .map(|value| value.to_string())
        .or_else(|| tab_state.borrow().tabs.first().map(|tab| tab.id.clone()));

    if let Some(active_tab_id) = active_tab_id {
        activate_tab(tab_strip, content_stack, tab_state, &active_tab_id);
    }
}

fn make_terminal_callbacks(
    tab_strip: &gtk::Box,
    content_stack: &gtk::Stack,
    tab_state: &Rc<RefCell<TabState>>,
    callbacks: &Rc<PaneCallbacks>,
    pane_outer: &gtk::Box,
    tab_id: &str,
    title_label: &gtk::Label,
    term_cwd: &Rc<RefCell<Option<String>>>,
) -> TerminalCallbacks {
    let title_label = title_label.clone();
    let state_for_title = tab_state.clone();
    let tid_for_title = tab_id.to_string();
    let tab_strip_for_close = tab_strip.clone();
    let content_stack_for_close = content_stack.clone();
    let state_for_close = tab_state.clone();
    let tid_for_close = tab_id.to_string();
    let callbacks_for_close = callbacks.clone();
    let pane_for_close = pane_outer.clone();
    let callbacks_for_pwd = callbacks.clone();
    let callbacks_for_notification = callbacks.clone();
    let callbacks_for_state = callbacks.clone();
    let callbacks_for_bell = callbacks.clone();
    let term_cwd_for_pwd = term_cwd.clone();

    TerminalCallbacks {
        on_title_changed: Box::new(move |title: &str| {
            let has_custom = state_for_title
                .borrow()
                .tabs
                .iter()
                .any(|entry| entry.id == tid_for_title && entry.custom_name.is_some());
            if has_custom {
                return;
            }
            if !title.is_empty() {
                let display = if title.len() > 22 {
                    format!("{}…", &title[..21])
                } else {
                    title.to_string()
                };
                title_label.set_label(&display);
            }
        }),
        on_bell: Box::new(move || {
            (callbacks_for_bell.on_bell)();
        }),
        on_desktop_notification: Box::new(move |title: &str, body: &str| {
            (callbacks_for_notification.on_desktop_notification)(title, body);
        }),
        on_pwd_changed: Box::new(move |pwd: &str| {
            *term_cwd_for_pwd.borrow_mut() = Some(pwd.to_string());
            (callbacks_for_pwd.on_pwd_changed)(pwd);
            (callbacks_for_state.on_state_changed)();
        }),
        on_close: Box::new(move || {
            let tab_strip = tab_strip_for_close.clone();
            let content_stack = content_stack_for_close.clone();
            let tab_state = state_for_close.clone();
            let tab_id = tid_for_close.clone();
            let callbacks = callbacks_for_close.clone();
            let pane_outer = pane_for_close.clone();
            glib::idle_add_local_once(move || {
                remove_tab(
                    &tab_strip,
                    &content_stack,
                    &tab_state,
                    &tab_id,
                    &callbacks,
                    &pane_outer,
                );
            });
        }),
        on_split_right: Box::new({
            let callbacks = callbacks.clone();
            let pane_outer = pane_outer.clone();
            move || {
                let widget: gtk::Widget = pane_outer.clone().upcast();
                (callbacks.on_split)(&widget, gtk::Orientation::Horizontal);
            }
        }),
        on_split_down: Box::new({
            let callbacks = callbacks.clone();
            let pane_outer = pane_outer.clone();
            move || {
                let widget: gtk::Widget = pane_outer.clone().upcast();
                (callbacks.on_split)(&widget, gtk::Orientation::Vertical);
            }
        }),
    }
}

fn add_terminal_tab_inner(
    tab_strip: &gtk::Box,
    content_stack: &gtk::Stack,
    tab_state: &Rc<RefCell<TabState>>,
    callbacks: &Rc<PaneCallbacks>,
    working_directory: Option<&str>,
    pane_outer: &gtk::Box,
    options: Option<TerminalTabOptions<'_>>,
) {
    let tab_id = options
        .as_ref()
        .and_then(|value| value.id.map(|id| id.to_string()))
        .unwrap_or_else(next_tab_id);

    let (tab_btn, title_label) = build_tab_button(
        "Terminal",
        &tab_id,
        tab_strip,
        content_stack,
        tab_state,
        callbacks,
        pane_outer,
    );

    // Build Ghostty terminal callbacks for title/bell/close
    let term_cwd = Rc::new(RefCell::new(
        options
            .as_ref()
            .and_then(|value| value.cwd.map(|cwd| cwd.to_string()))
            .or_else(|| working_directory.map(|cwd| cwd.to_string())),
    ));
    let term_callbacks = make_terminal_callbacks(
        tab_strip,
        content_stack,
        tab_state,
        callbacks,
        pane_outer,
        &tab_id,
        &title_label,
        &term_cwd,
    );

    let term = terminal::create_terminal(working_directory, term_callbacks);

    let widget: gtk::Widget = term.clone().upcast();
    content_stack.add_named(&widget, Some(&tab_id));

    {
        let mut ts = tab_state.borrow_mut();
        ts.tabs.push(TabEntry {
            id: tab_id.clone(),
            tab_button: tab_btn,
            title_label: title_label.clone(),
            content: widget,
            custom_name: options
                .as_ref()
                .and_then(|value| value.custom_name.map(|name| name.to_string())),
            pinned: options.as_ref().map(|value| value.pinned).unwrap_or(false),
            kind: TabKind::Terminal {
                cwd: term_cwd.clone(),
            },
        });
    }

    if let Some(custom_name) = options.as_ref().and_then(|value| value.custom_name) {
        title_label.set_label(custom_name);
    }
    if options.as_ref().map(|value| value.pinned).unwrap_or(false) {
        if let Some(entry) = tab_state
            .borrow()
            .tabs
            .iter()
            .find(|entry| entry.id == tab_id)
        {
            apply_pin_visuals(&entry.tab_button, true);
        }
    }

    activate_tab(tab_strip, content_stack, tab_state, &tab_id);
    term.grab_focus();
    if options.is_none() {
        (callbacks.on_state_changed)();
    }
}

fn add_browser_tab_inner(
    tab_strip: &gtk::Box,
    content_stack: &gtk::Stack,
    tab_state: &Rc<RefCell<TabState>>,
    callbacks: &Rc<PaneCallbacks>,
    pane_outer: &gtk::Box,
    options: Option<BrowserTabOptions<'_>>,
) {
    let tab_id = options
        .as_ref()
        .and_then(|value| value.id.map(|id| id.to_string()))
        .unwrap_or_else(next_tab_id);
    let saved_uri = Rc::new(RefCell::new(
        options
            .as_ref()
            .and_then(|value| value.uri.map(|uri| uri.to_string())),
    ));
    let (widget, title) = create_browser_widget(
        options.as_ref().and_then(|value| value.uri),
        saved_uri.clone(),
        callbacks.clone(),
    );

    let (tab_btn, title_label) = build_tab_button(
        &title,
        &tab_id,
        tab_strip,
        content_stack,
        tab_state,
        callbacks,
        pane_outer,
    );

    content_stack.add_named(&widget, Some(&tab_id));

    {
        let mut ts = tab_state.borrow_mut();
        ts.tabs.push(TabEntry {
            id: tab_id.clone(),
            tab_button: tab_btn,
            title_label: title_label.clone(),
            content: widget,
            custom_name: options
                .as_ref()
                .and_then(|value| value.custom_name.map(|name| name.to_string())),
            pinned: options.as_ref().map(|value| value.pinned).unwrap_or(false),
            kind: TabKind::Browser {
                uri: saved_uri.clone(),
            },
        });
    }

    if let Some(custom_name) = options.as_ref().and_then(|value| value.custom_name) {
        title_label.set_label(custom_name);
    }
    if options.as_ref().map(|value| value.pinned).unwrap_or(false) {
        if let Some(entry) = tab_state
            .borrow()
            .tabs
            .iter()
            .find(|entry| entry.id == tab_id)
        {
            apply_pin_visuals(&entry.tab_button, true);
        }
    }

    activate_tab(tab_strip, content_stack, tab_state, &tab_id);
    if options.is_none() {
        (callbacks.on_state_changed)();
    }
}

// Public wrappers for keyboard shortcut use
#[allow(dead_code)]
pub fn add_terminal_tab_to_pane(pane_widget: &gtk::Widget) {
    if let Some(internals) = find_pane_internals(pane_widget) {
        let dir = internals.working_directory.borrow().clone();
        add_terminal_tab_inner(
            &internals.tab_strip,
            &internals.content_stack,
            &internals.tab_state,
            &internals.callbacks,
            dir.as_deref(),
            &internals.pane_outer,
            None,
        );
    }
}

#[allow(dead_code)]
pub fn add_browser_tab_to_pane(pane_widget: &gtk::Widget) {
    if let Some(internals) = find_pane_internals(pane_widget) {
        add_browser_tab_inner(
            &internals.tab_strip,
            &internals.content_stack,
            &internals.tab_state,
            &internals.callbacks,
            &internals.pane_outer,
            None,
        );
    }
}

pub fn tab_title(pane_widget: &gtk::Widget, tab_id: &str) -> Option<String> {
    let internals = find_pane_internals(pane_widget)?;
    let tab_state = internals.tab_state.borrow();
    let entry = tab_state.tabs.iter().find(|entry| entry.id == tab_id)?;
    Some(entry.title_label.label().to_string())
}

pub fn tab_working_directory(pane_widget: &gtk::Widget, tab_id: &str) -> Option<String> {
    let internals = find_pane_internals(pane_widget)?;
    let tab_state = internals.tab_state.borrow();
    let entry = tab_state.tabs.iter().find(|entry| entry.id == tab_id)?;
    match &entry.kind {
        TabKind::Terminal { cwd } => cwd.borrow().clone(),
        TabKind::Browser { .. } => None,
    }
}

pub fn move_tab_to_pane(
    source_pane: &gtk::Widget,
    tab_id: &str,
    target_pane: &gtk::Widget,
) -> bool {
    let Some(source) = find_pane_internals(source_pane) else {
        return false;
    };
    let Some(target) = find_pane_internals(target_pane) else {
        return false;
    };
    let insert_idx = target.tab_state.borrow().tabs.len();
    transfer_tab_between_panes(&source, &target, tab_id, insert_idx)
}

pub fn snapshot_pane_state(pane_widget: &gtk::Widget) -> Option<PaneState> {
    let internals = find_pane_internals(pane_widget)?;
    let ts = internals.tab_state.borrow();
    let tabs = ts
        .tabs
        .iter()
        .map(|entry| {
            let content = match &entry.kind {
                TabKind::Terminal { cwd } => TabContentState::Terminal {
                    cwd: cwd.borrow().clone(),
                },
                TabKind::Browser { uri } => TabContentState::Browser {
                    uri: uri.borrow().clone(),
                },
            };
            SavedTabState {
                id: entry.id.clone(),
                custom_name: entry.custom_name.clone(),
                pinned: entry.pinned,
                content,
            }
        })
        .collect();
    Some(PaneState {
        active_tab_id: ts.active_tab.clone(),
        tabs,
    })
}

#[allow(dead_code)]
fn find_pane_internals(pane_widget: &gtk::Widget) -> Option<Rc<PaneInternals>> {
    let outer = pane_widget.downcast_ref::<gtk::Box>()?;
    unsafe {
        outer
            .data::<Rc<PaneInternals>>("limux-pane-internals")
            .map(|ptr| ptr.as_ref().clone())
    }
}

fn apply_pin_visuals(tab_button: &gtk::Box, pinned: bool) {
    if let Some(close_widget) = tab_button.last_child() {
        close_widget.set_visible(!pinned);
    }
    if let Some(inner_box) = tab_button
        .first_child()
        .and_then(|child| child.downcast::<gtk::Box>().ok())
    {
        if let Some(pin_icon) = inner_box
            .first_child()
            .and_then(|child| child.downcast::<gtk::Label>().ok())
        {
            pin_icon.set_label(if pinned { "📌" } else { "" });
            pin_icon.set_visible(pinned);
        }
    }
}

// ---------------------------------------------------------------------------
// Tab button (label + close)
// ---------------------------------------------------------------------------

fn new_tab_title_label(title: &str) -> gtk::Label {
    let label = gtk::Label::builder()
        .label(title)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .max_width_chars(20)
        .build();
    label.set_can_target(false);
    label
}

fn build_tab_button(
    title: &str,
    tab_id: &str,
    tab_strip: &gtk::Box,
    content_stack: &gtk::Stack,
    tab_state: &Rc<RefCell<TabState>>,
    callbacks: &Rc<PaneCallbacks>,
    pane_outer: &gtk::Box,
) -> (gtk::Box, gtk::Label) {
    let label = new_tab_title_label(title);
    let tab_button = build_tab_button_from_label(
        &label,
        tab_id,
        tab_strip,
        content_stack,
        tab_state,
        callbacks,
        pane_outer,
    );
    (tab_button, label)
}

fn build_tab_button_from_label(
    label: &gtk::Label,
    tab_id: &str,
    tab_strip: &gtk::Box,
    content_stack: &gtk::Stack,
    tab_state: &Rc<RefCell<TabState>>,
    callbacks: &Rc<PaneCallbacks>,
    pane_outer: &gtk::Box,
) -> gtk::Box {
    if let Some(parent) = label
        .parent()
        .and_then(|parent| parent.downcast::<gtk::Box>().ok())
    {
        parent.remove(label);
    }

    let pin_icon = gtk::Label::new(None);
    pin_icon.add_css_class("limux-pin-icon");
    pin_icon.set_visible(false);
    pin_icon.set_can_target(false);

    let close_btn = gtk::Button::builder()
        .icon_name("window-close-symbolic")
        .has_frame(false)
        .build();
    close_btn.add_css_class("limux-tab-close");

    let inner_box = gtk::Box::new(gtk::Orientation::Horizontal, 2);
    inner_box.set_can_target(false);
    inner_box.append(&pin_icon);
    inner_box.append(label);

    let tab_btn = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    tab_btn.add_css_class("limux-tab");
    tab_btn.append(&inner_box);
    tab_btn.append(&close_btn);

    let click = gtk::GestureClick::new();
    click.set_button(1);
    {
        let tid = tab_id.to_string();
        let ts = tab_strip.clone();
        let cs = content_stack.clone();
        let state = tab_state.clone();
        let callbacks = callbacks.clone();
        click.connect_pressed(move |_, _, _, _| {
            activate_tab(&ts, &cs, &state, &tid);
            (callbacks.on_state_changed)();
        });
    }
    tab_btn.add_controller(click);

    let right_click = gtk::GestureClick::new();
    right_click.set_button(3);
    {
        let tid = tab_id.to_string();
        let ts = tab_strip.clone();
        let cs = content_stack.clone();
        let state = tab_state.clone();
        let cb = callbacks.clone();
        let po = pane_outer.clone();
        let lbl = label.clone();
        let pin = pin_icon.clone();
        let tb = tab_btn.clone();
        let context = TabContextMenuContext {
            tab_strip: ts,
            content_stack: cs,
            tab_state: state,
            callbacks: cb,
            pane_outer: po,
            label: lbl,
            pin_icon: pin,
        };
        right_click.connect_pressed(move |_gesture, _, _x, _y| {
            show_tab_context_menu(&tb, &tid, &context);
        });
    }
    tab_btn.add_controller(right_click);

    let pane_id = unsafe {
        pane_outer
            .data::<u32>("limux-pane-id")
            .map(|value| *value.as_ref())
            .unwrap_or(0)
    };
    let drag_source = gtk::DragSource::new();
    drag_source.set_actions(gtk::gdk::DragAction::MOVE);
    {
        let tid = tab_id.to_string();
        drag_source.connect_prepare(move |_src, _x, _y| {
            let val = glib::Value::from(&TabDragPayload::new(pane_id, &tid).encode());
            Some(gtk::gdk::ContentProvider::for_value(&val))
        });
    }
    drag_source.connect_drag_begin(move |source, _| {
        set_tab_dragging(true);
        if let Some(widget) = source.widget() {
            let icon = gtk::WidgetPaintable::new(Some(&widget));
            source.set_icon(Some(&icon), 0, 0);
        }
    });
    drag_source.connect_drag_end(move |_, _, _| {
        set_tab_dragging(false);
    });
    tab_btn.add_controller(drag_source);

    {
        let tid = tab_id.to_string();
        let ts = tab_strip.clone();
        let cs = content_stack.clone();
        let state = tab_state.clone();
        let cb = callbacks.clone();
        let po = pane_outer.clone();
        close_btn.connect_clicked(move |_| {
            let is_pinned = state.borrow().tabs.iter().any(|e| e.id == tid && e.pinned);
            if !is_pinned {
                remove_tab(&ts, &cs, &state, &tid, &cb, &po);
            }
        });
    }

    tab_strip.append(&tab_btn);

    tab_btn
}

fn show_tab_context_menu(tab_btn: &gtk::Box, tab_id: &str, context: &TabContextMenuContext) {
    let menu = gtk::PopoverMenu::from_model(None::<&gtk::gio::MenuModel>);
    let menu_box = gtk::Box::new(gtk::Orientation::Vertical, 2);
    menu_box.set_margin_top(4);
    menu_box.set_margin_bottom(4);
    menu_box.set_margin_start(4);
    menu_box.set_margin_end(4);

    // Rename
    let rename_btn = gtk::Button::with_label("Rename");
    rename_btn.add_css_class("flat");
    {
        let lbl = context.label.clone();
        let state = context.tab_state.clone();
        let tid = tab_id.to_string();
        let menu_ref = menu.clone();
        let callbacks = context.callbacks.clone();
        rename_btn.connect_clicked(move |_| {
            menu_ref.popdown();
            show_rename_dialog(&lbl, &state, &tid, &callbacks);
        });
    }

    // Pin / Unpin
    let is_pinned = context
        .tab_state
        .borrow()
        .tabs
        .iter()
        .any(|e| e.id == tab_id && e.pinned);
    let pin_label = if is_pinned { "Unpin" } else { "Pin" };
    let pin_btn = gtk::Button::with_label(pin_label);
    pin_btn.add_css_class("flat");
    {
        let state = context.tab_state.clone();
        let tid = tab_id.to_string();
        let pin = context.pin_icon.clone();
        let close = tab_btn.last_child(); // close button
        let menu_ref = menu.clone();
        let callbacks = context.callbacks.clone();
        pin_btn.connect_clicked(move |_| {
            menu_ref.popdown();
            let mut ts = state.borrow_mut();
            if let Some(entry) = ts.find_tab_mut(&tid) {
                entry.pinned = !entry.pinned;
                apply_pin_visuals(&entry.tab_button, entry.pinned);
                pin.set_label(if entry.pinned { "📌" } else { "" });
                pin.set_visible(entry.pinned);
                if let Some(close_widget) = &close {
                    close_widget.set_visible(!entry.pinned);
                }
            }
            drop(ts);
            (callbacks.on_state_changed)();
        });
    }

    // Close
    let close_btn = gtk::Button::with_label("Close");
    close_btn.add_css_class("flat");
    {
        let tid = tab_id.to_string();
        let ts = context.tab_strip.clone();
        let cs = context.content_stack.clone();
        let state = context.tab_state.clone();
        let cb = context.callbacks.clone();
        let po = context.pane_outer.clone();
        let menu_ref = menu.clone();
        close_btn.connect_clicked(move |_| {
            menu_ref.popdown();
            remove_tab(&ts, &cs, &state, &tid, &cb, &po);
        });
    }

    menu_box.append(&rename_btn);
    menu_box.append(&pin_btn);
    menu_box.append(&close_btn);
    menu.set_child(Some(&menu_box));
    menu.set_parent(tab_btn);
    menu.set_has_arrow(false);

    // Clean up popover when it closes
    menu.connect_closed(move |popover| {
        popover.unparent();
    });

    menu.popup();
}

fn show_rename_dialog(
    label: &gtk::Label,
    tab_state: &Rc<RefCell<TabState>>,
    tab_id: &str,
    callbacks: &Rc<PaneCallbacks>,
) {
    let current_name = label.label().to_string();

    // Replace label with an entry temporarily
    let parent = label.parent().and_then(|p| p.downcast::<gtk::Box>().ok());
    let Some(parent) = parent else {
        return;
    };

    let entry = gtk::Entry::builder()
        .text(&current_name)
        .width_chars(15)
        .build();
    entry.add_css_class("limux-tab-rename-entry");

    label.set_visible(false);
    // Insert entry before the close button
    parent.insert_child_after(&entry, Some(label));
    entry.grab_focus();
    entry.select_region(0, -1);

    // On activate (Enter) or focus-out, commit rename
    let lbl = label.clone();
    let state = tab_state.clone();
    let tid = tab_id.to_string();
    let parent_for_cleanup = parent.clone();

    let commit = Rc::new(std::cell::Cell::new(false));

    let do_rename = {
        let commit = commit.clone();
        let lbl = lbl.clone();
        let state = state.clone();
        let tid = tid.clone();
        let parent = parent_for_cleanup.clone();
        let callbacks = callbacks.clone();
        move |entry: &gtk::Entry| {
            if commit.get() {
                return;
            }
            commit.set(true);
            let new_name = entry.text().to_string();
            if !new_name.trim().is_empty() {
                lbl.set_label(&new_name);
                let mut ts = state.borrow_mut();
                if let Some(tab) = ts.find_tab_mut(&tid) {
                    tab.custom_name = Some(new_name);
                }
            }
            lbl.set_visible(true);
            parent.remove(entry);
            (callbacks.on_state_changed)();
        }
    };

    {
        let do_rename = do_rename.clone();
        entry.connect_activate(move |e| {
            do_rename(e);
        });
    }
    {
        let do_rename = do_rename.clone();
        let focus_controller = gtk::EventControllerFocus::new();
        focus_controller.connect_leave(move |ctrl| {
            if let Some(widget) = ctrl.widget() {
                if let Some(entry) = widget.downcast_ref::<gtk::Entry>() {
                    do_rename(entry);
                }
            }
        });
        entry.add_controller(focus_controller);
    }
}

fn normalize_reorder_insert_index(source_idx: usize, insert_idx: usize) -> Option<usize> {
    if source_idx == insert_idx || source_idx + 1 == insert_idx {
        return None;
    }
    Some(if source_idx < insert_idx {
        insert_idx - 1
    } else {
        insert_idx
    })
}

fn next_active_after_tab_removal(
    tab_ids: &[&str],
    active_id: Option<&str>,
    removed_idx: usize,
) -> Option<String> {
    if tab_ids.len() <= 1 {
        return None;
    }
    let removed_id = tab_ids.get(removed_idx).copied()?;
    if active_id != Some(removed_id) {
        return active_id.map(ToOwned::to_owned);
    }
    let next_idx = removed_idx.min(tab_ids.len() - 2);
    tab_ids
        .iter()
        .enumerate()
        .filter_map(|(idx, tab_id)| (idx != removed_idx).then_some(*tab_id))
        .nth(next_idx)
        .map(ToOwned::to_owned)
}

fn classify_content_drop_zone(width: f64, height: f64, x: f64, y: f64) -> Option<ContentDropZone> {
    if width <= 0.0 || height <= 0.0 {
        return None;
    }
    if x < width * 0.25 {
        Some(ContentDropZone::Left)
    } else if x > width * 0.75 {
        Some(ContentDropZone::Right)
    } else if y < height * 0.25 {
        Some(ContentDropZone::Top)
    } else if y > height * 0.75 {
        Some(ContentDropZone::Bottom)
    } else {
        Some(ContentDropZone::Center)
    }
}

fn content_drop_preview_rect(zone: ContentDropZone) -> (f64, f64, f64, f64) {
    match zone {
        ContentDropZone::Left => (0.0, 0.0, 0.5, 1.0),
        ContentDropZone::Right => (0.5, 0.0, 0.5, 1.0),
        ContentDropZone::Top => (0.0, 0.0, 1.0, 0.5),
        ContentDropZone::Bottom => (0.0, 0.5, 1.0, 0.5),
        ContentDropZone::Center => (0.25, 0.25, 0.5, 0.5),
    }
}

fn clear_content_drop_zone(overlay: &gtk::Box) {
    overlay.remove_css_class("limux-drop-preview");
    overlay.remove_css_class("limux-drop-preview-center");
    overlay.set_size_request(-1, -1);
    overlay.set_margin_start(0);
    overlay.set_margin_top(0);
}

fn highlight_content_drop_zone(overlay: &gtk::Box, zone: ContentDropZone) {
    clear_content_drop_zone(overlay);
    overlay.add_css_class("limux-drop-preview");
    if zone == ContentDropZone::Center {
        overlay.add_css_class("limux-drop-preview-center");
    }
    let (x_frac, y_frac, width_frac, height_frac) = content_drop_preview_rect(zone);
    let total_width = overlay
        .parent()
        .map(|parent| parent.allocation().width())
        .unwrap_or_else(|| overlay.width())
        .max(1);
    let total_height = overlay
        .parent()
        .map(|parent| parent.allocation().height())
        .unwrap_or_else(|| overlay.height())
        .max(1);
    overlay.set_margin_start((total_width as f64 * x_frac).round() as i32);
    overlay.set_margin_top((total_height as f64 * y_frac).round() as i32);
    overlay.set_size_request(
        (total_width as f64 * width_frac).round() as i32,
        (total_height as f64 * height_frac).round() as i32,
    );
}

fn insert_index_for_drop(
    tab_state: &Rc<RefCell<TabState>>,
    x: f64,
    ignored_tab_id: Option<&str>,
) -> usize {
    let tab_state = tab_state.borrow();
    for (idx, entry) in tab_state.tabs.iter().enumerate() {
        if ignored_tab_id == Some(entry.id.as_str()) {
            continue;
        }
        let allocation = entry.tab_button.allocation();
        let midpoint = allocation.x() as f64 + allocation.width() as f64 / 2.0;
        if x < midpoint {
            return idx;
        }
    }
    tab_state.tabs.len()
}

fn rebuild_tab_strip(tab_strip: &gtk::Box, tab_state: &Rc<RefCell<TabState>>) {
    let buttons: Vec<gtk::Box> = tab_state
        .borrow()
        .tabs
        .iter()
        .map(|entry| entry.tab_button.clone())
        .collect();
    for button in &buttons {
        if button.parent().is_some() {
            tab_strip.remove(button);
        }
    }
    for button in &buttons {
        tab_strip.append(button);
    }
}

fn rebind_moved_tab_entry(entry: &mut TabEntry, target: &Rc<PaneInternals>) {
    if let TabKind::Terminal { cwd } = &entry.kind {
        let callbacks = make_terminal_callbacks(
            &target.tab_strip,
            &target.content_stack,
            &target.tab_state,
            &target.callbacks,
            &target.pane_outer,
            &entry.id,
            &entry.title_label,
            cwd,
        );
        let _ = terminal::replace_callbacks(&entry.content, callbacks);
    }
    entry.tab_button = build_tab_button_from_label(
        &entry.title_label,
        &entry.id,
        &target.tab_strip,
        &target.content_stack,
        &target.tab_state,
        &target.callbacks,
        &target.pane_outer,
    );
    if entry.pinned {
        apply_pin_visuals(&entry.tab_button, true);
    }
}

fn reorder_tab(
    tab_strip: &gtk::Box,
    tab_state: &Rc<RefCell<TabState>>,
    source_id: &str,
    insert_idx: usize,
    callbacks: &Rc<PaneCallbacks>,
) -> bool {
    let mut tab_state_ref = tab_state.borrow_mut();
    let Some(source_idx) = tab_state_ref
        .tabs
        .iter()
        .position(|entry| entry.id == source_id)
    else {
        return false;
    };
    let Some(normalized_idx) = normalize_reorder_insert_index(source_idx, insert_idx) else {
        return false;
    };
    let entry = tab_state_ref.tabs.remove(source_idx);
    tab_state_ref.tabs.insert(normalized_idx, entry);
    drop(tab_state_ref);
    rebuild_tab_strip(tab_strip, tab_state);
    (callbacks.on_state_changed)();
    true
}

fn transfer_tab_between_panes(
    source: &Rc<PaneInternals>,
    target: &Rc<PaneInternals>,
    tab_id: &str,
    insert_idx: usize,
) -> bool {
    if source.pane_id == target.pane_id {
        return false;
    }

    let (mut entry, source_next_active) = {
        let mut source_state = source.tab_state.borrow_mut();
        let Some(source_idx) = source_state.tabs.iter().position(|item| item.id == tab_id) else {
            return false;
        };
        let all_ids: Vec<&str> = source_state
            .tabs
            .iter()
            .map(|item| item.id.as_str())
            .collect();
        let next_active =
            next_active_after_tab_removal(&all_ids, source_state.active_tab.as_deref(), source_idx);
        (source_state.tabs.remove(source_idx), next_active)
    };

    if entry.tab_button.parent().is_some() {
        source.tab_strip.remove(&entry.tab_button);
    }
    if entry.content.parent().is_some() {
        source.content_stack.remove(&entry.content);
    }

    rebind_moved_tab_entry(&mut entry, target);
    let moved_tab_id = entry.id.clone();
    target
        .content_stack
        .add_named(&entry.content, Some(&moved_tab_id));

    {
        let mut target_state = target.tab_state.borrow_mut();
        let clamped_idx = insert_idx.min(target_state.tabs.len());
        target_state.tabs.insert(clamped_idx, entry);
    }
    rebuild_tab_strip(&target.tab_strip, &target.tab_state);

    if source.tab_state.borrow().tabs.is_empty() {
        (source.callbacks.on_empty)(&source.pane_outer.clone().upcast());
    } else if let Some(next_active) = source_next_active {
        activate_tab(
            &source.tab_strip,
            &source.content_stack,
            &source.tab_state,
            &next_active,
        );
    }

    activate_tab(
        &target.tab_strip,
        &target.content_stack,
        &target.tab_state,
        &moved_tab_id,
    );
    (target.callbacks.on_state_changed)();
    true
}

fn install_tab_strip_drop_target(tab_strip: &gtk::Box, internals: &Rc<PaneInternals>) {
    let drop_target = gtk::DropTarget::new(glib::Type::STRING, gtk::gdk::DragAction::MOVE);
    {
        let target = internals.clone();
        drop_target.connect_drop(move |_, value, x, _| {
            let Ok(raw) = value.get::<String>() else {
                return false;
            };
            let Some(payload) = TabDragPayload::decode(&raw) else {
                return false;
            };
            let same_pane = payload.pane_id == target.pane_id;
            let insert_idx = insert_index_for_drop(
                &target.tab_state,
                x,
                same_pane.then_some(payload.tab_id.as_str()),
            );
            if same_pane {
                return reorder_tab(
                    &target.tab_strip,
                    &target.tab_state,
                    &payload.tab_id,
                    insert_idx,
                    &target.callbacks,
                );
            }
            let Some(source) = lookup_pane_internals(payload.pane_id) else {
                return false;
            };
            transfer_tab_between_panes(&source, &target, &payload.tab_id, insert_idx)
        });
    }
    tab_strip.add_controller(drop_target);
}

fn set_browser_targeting_enabled(content_stack: &gtk::Stack, enabled: bool) {
    let mut child = content_stack.first_child();
    while let Some(widget) = child {
        child = widget.next_sibling();
        if !widget.has_css_class("limux-browser") {
            continue;
        }
        if let Some(webview) = widget.last_child() {
            webview.set_can_target(enabled);
        }
    }
}

fn install_content_drop_target(content_overlay: &gtk::Overlay, internals: &Rc<PaneInternals>) {
    let drop_target = gtk::DropTarget::new(glib::Type::STRING, gtk::gdk::DragAction::MOVE);
    drop_target.set_preload(true);
    {
        let overlay = internals.content_drop_overlay.clone();
        let content_stack = internals.content_stack.clone();
        let workspace_dragging = internals.workspace_dragging.clone();
        drop_target.connect_motion(move |_, x, y| {
            if workspace_dragging.get() || !is_tab_dragging() {
                clear_content_drop_zone(&overlay);
                return gtk::gdk::DragAction::empty();
            }
            let width = content_stack.allocation().width().max(overlay.width()) as f64;
            let height = content_stack.allocation().height().max(overlay.height()) as f64;
            let Some(zone) = classify_content_drop_zone(width, height, x, y) else {
                clear_content_drop_zone(&overlay);
                return gtk::gdk::DragAction::empty();
            };
            highlight_content_drop_zone(&overlay, zone);
            gtk::gdk::DragAction::MOVE
        });
    }
    {
        let overlay = internals.content_drop_overlay.clone();
        drop_target.connect_leave(move |_| {
            clear_content_drop_zone(&overlay);
        });
    }
    {
        let target = internals.clone();
        let overlay = internals.content_drop_overlay.clone();
        let content_stack = internals.content_stack.clone();
        drop_target.connect_drop(move |_, value, x, y| {
            clear_content_drop_zone(&overlay);
            let Ok(raw) = value.get::<String>() else {
                return false;
            };
            let Some(payload) = TabDragPayload::decode(&raw) else {
                return false;
            };
            if payload.pane_id == target.pane_id {
                return false;
            }
            let width = content_stack.allocation().width().max(overlay.width()) as f64;
            let height = content_stack.allocation().height().max(overlay.height()) as f64;
            let Some(zone) = classify_content_drop_zone(width, height, x, y) else {
                return false;
            };
            match zone {
                ContentDropZone::Center => {
                    let Some(source) = lookup_pane_internals(payload.pane_id) else {
                        return false;
                    };
                    transfer_tab_between_panes(
                        &source,
                        &target,
                        &payload.tab_id,
                        target.tab_state.borrow().tabs.len(),
                    )
                }
                ContentDropZone::Left
                | ContentDropZone::Right
                | ContentDropZone::Top
                | ContentDropZone::Bottom => {
                    let Some(source_widget) = find_pane_widget_by_id(payload.pane_id) else {
                        return false;
                    };
                    let target_widget: gtk::Widget = target.pane_outer.clone().upcast();
                    let (orientation, new_pane_first) = match zone {
                        ContentDropZone::Left => (gtk::Orientation::Horizontal, true),
                        ContentDropZone::Right => (gtk::Orientation::Horizontal, false),
                        ContentDropZone::Top => (gtk::Orientation::Vertical, true),
                        ContentDropZone::Bottom => (gtk::Orientation::Vertical, false),
                        ContentDropZone::Center => unreachable!(),
                    };
                    (target.callbacks.on_split_with_tab)(
                        &source_widget,
                        &target_widget,
                        orientation,
                        payload.tab_id.clone(),
                        new_pane_first,
                    );
                    true
                }
            }
        });
    }
    content_overlay.add_controller(drop_target);

    let overlay = internals.content_drop_overlay.clone();
    let content_stack = internals.content_stack.clone();
    let workspace_dragging = internals.workspace_dragging.clone();
    let listener_id = on_tab_drag_change(move |dragging| {
        let visible = dragging && !workspace_dragging.get();
        overlay.set_visible(visible);
        if !visible {
            clear_content_drop_zone(&overlay);
        }
        set_browser_targeting_enabled(&content_stack, !dragging);
    });
    internals.pane_outer.connect_destroy(move |_| {
        remove_tab_drag_listener(listener_id);
    });
}

// ---------------------------------------------------------------------------
// Tab activation / removal
// ---------------------------------------------------------------------------

fn activate_tab(
    _tab_strip: &gtk::Box,
    content_stack: &gtk::Stack,
    tab_state: &Rc<RefCell<TabState>>,
    tab_id: &str,
) {
    let mut ts = tab_state.borrow_mut();
    if ts.active_tab.as_deref() == Some(tab_id) {
        return;
    }
    ts.active_tab = Some(tab_id.to_string());

    // Update visual state on all tabs
    for entry in &ts.tabs {
        if entry.id == tab_id {
            entry.tab_button.add_css_class("limux-tab-active");
        } else {
            entry.tab_button.remove_css_class("limux-tab-active");
        }
    }

    content_stack.set_visible_child_name(tab_id);

    // Focus the content — only grab focus on directly focusable widgets (terminals).
    // For containers (browser vbox), focus the first focusable child instead.
    if let Some(entry) = ts.tabs.iter().find(|e| e.id == tab_id) {
        let content = entry.content.clone();
        drop(ts);
        if content.is_focus() || content.can_focus() {
            content.grab_focus();
        } else {
            // Try to find a focusable child (e.g., the WebView inside a Box)
            content.child_focus(gtk::DirectionType::TabForward);
        }
    }
}

fn remove_tab(
    tab_strip: &gtk::Box,
    content_stack: &gtk::Stack,
    tab_state: &Rc<RefCell<TabState>>,
    tab_id: &str,
    callbacks: &Rc<PaneCallbacks>,
    pane_outer: &gtk::Box,
) {
    let mut ts = tab_state.borrow_mut();
    let Some(idx) = ts.tabs.iter().position(|e| e.id == tab_id) else {
        return;
    };
    let entry = ts.tabs.remove(idx);

    tab_strip.remove(&entry.tab_button);
    content_stack.remove(&entry.content);

    if ts.tabs.is_empty() {
        drop(ts);
        (callbacks.on_empty)(&pane_outer.clone().upcast());
        return;
    }

    // Activate neighbor tab
    let new_idx = idx.min(ts.tabs.len() - 1);
    let new_id = ts.tabs[new_idx].id.clone();
    let was_active = ts.active_tab.as_deref() == Some(tab_id);
    drop(ts);

    if was_active {
        activate_tab(tab_strip, content_stack, tab_state, &new_id);
    }
    (callbacks.on_state_changed)();
}

// ---------------------------------------------------------------------------
// Browser widget
// ---------------------------------------------------------------------------

#[cfg(feature = "webkit")]
fn create_browser_widget(
    initial_uri: Option<&str>,
    saved_uri: Rc<RefCell<Option<String>>>,
    callbacks: Rc<PaneCallbacks>,
) -> (gtk::Widget, String) {
    use webkit6::prelude::*;

    // Use a NetworkSession to avoid sandbox issues
    let network_session = webkit6::NetworkSession::default();
    let web_context = webkit6::WebContext::default();

    let webview = webkit6::WebView::builder()
        .hexpand(true)
        .vexpand(true)
        .build();

    // Set permissive settings
    if let Some(settings) = webkit6::prelude::WebViewExt::settings(&webview) {
        settings.set_enable_developer_extras(true);
        settings.set_javascript_can_open_windows_automatically(true);
    }

    let url_entry = gtk::Entry::builder()
        .placeholder_text("Enter URL...")
        .hexpand(true)
        .build();

    let back_btn = icon_button("go-previous-symbolic", "Back");
    let fwd_btn = icon_button("go-next-symbolic", "Forward");
    let reload_btn = icon_button("view-refresh-symbolic", "Reload");

    let nav_bar = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    nav_bar.add_css_class("limux-pane-header");
    nav_bar.append(&back_btn);
    nav_bar.append(&fwd_btn);
    nav_bar.append(&reload_btn);
    nav_bar.append(&url_entry);

    {
        let wv = webview.clone();
        back_btn.connect_clicked(move |_| {
            wv.go_back();
        });
    }
    {
        let wv = webview.clone();
        fwd_btn.connect_clicked(move |_| {
            wv.go_forward();
        });
    }
    {
        let wv = webview.clone();
        reload_btn.connect_clicked(move |_| {
            wv.reload();
        });
    }
    {
        let wv = webview.clone();
        url_entry.connect_activate(move |entry| {
            let mut url = entry.text().to_string();
            if !url.starts_with("http://") && !url.starts_with("https://") {
                if url.contains('.') {
                    url = format!("https://{url}");
                } else {
                    url = format!("https://www.google.com/search?q={}", url.replace(' ', "+"));
                }
            }
            wv.load_uri(&url);
        });
    }
    {
        let entry = url_entry.clone();
        let saved_uri = saved_uri.clone();
        let callbacks = callbacks.clone();
        let restoring = Rc::new(std::cell::Cell::new(initial_uri.is_some()));
        let restoring_flag = restoring.clone();
        webview.connect_uri_notify(move |wv| {
            if let Some(uri) = wv.uri() {
                let uri_str: String = uri.into();
                entry.set_text(&uri_str);
                if restoring_flag.get() && (uri_str.is_empty() || uri_str == "about:blank") {
                    return;
                }
                restoring_flag.set(false);
                *saved_uri.borrow_mut() = Some(uri_str);
                (callbacks.on_state_changed)();
            }
        });
    }

    let vbox = gtk::Box::new(gtk::Orientation::Vertical, 0);
    vbox.add_css_class("limux-browser");
    vbox.append(&nav_bar);
    vbox.append(&webview.clone());
    vbox.set_hexpand(true);
    vbox.set_vexpand(true);

    // Load default URL only on the first map. The WebView preserves its
    // page and history across reparenting (splits), so we must not reload.
    {
        let wv = webview.clone();
        let loaded = std::cell::Cell::new(false);
        let initial_uri = initial_uri.map(|value| value.to_string());
        vbox.connect_map(move |_| {
            if !loaded.get() {
                loaded.set(true);
                if let Some(uri) = &initial_uri {
                    wv.load_uri(uri);
                } else {
                    wv.load_uri("https://google.com");
                }
            }
        });
    }

    // Suppress unused variable warnings
    let _ = network_session;
    let _ = web_context;

    (vbox.upcast(), "Browser".to_string())
}

#[cfg(not(feature = "webkit"))]
fn create_browser_widget(
    initial_uri: Option<&str>,
    saved_uri: Rc<RefCell<Option<String>>>,
    _callbacks: Rc<PaneCallbacks>,
) -> (gtk::Widget, String) {
    *saved_uri.borrow_mut() = initial_uri.map(|value| value.to_string());
    let placeholder = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Center)
        .spacing(12)
        .build();

    let msg = gtk::Label::builder()
        .label("Browser requires webkit6")
        .build();
    msg.set_css_classes(&["dim-label"]);

    let hint = gtk::Label::builder()
        .label("sudo apt install libwebkitgtk-6.0-dev\ncargo build --features webkit")
        .justify(gtk::Justification::Center)
        .build();
    hint.set_css_classes(&["dim-label"]);

    placeholder.append(&msg);
    placeholder.append(&hint);
    placeholder.set_hexpand(true);
    placeholder.set_vexpand(true);

    (placeholder.upcast(), "Browser".to_string())
}
