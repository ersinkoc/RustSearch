use crate::theme::{self, Palette};
use gpui::{
    ClipboardItem, Context, Entity, FocusHandle, IntoElement, PathPromptOptions, Render,
    ScrollStrategy, Subscription, UniformListScrollHandle, Window, actions, div, prelude::*, px,
    relative, uniform_list,
};
use gpui_component::{
    Disableable, Icon, IconName, Sizable, TitleBar,
    button::{Button, ButtonVariants},
    input::{Input, InputEvent, InputState},
    menu::{DropdownMenu, PopupMenuItem},
    spinner::Spinner,
};
use rustsearch_desktop::engine::{Engine, Event, Request, Row, SearchSpec, SourceInfo};
use rustsearch_desktop::presentation::display_path;
use rustsearch_query::{Kind, Mode};
use serde::{Deserialize, Serialize};
use std::{path::PathBuf, sync::Arc};

actions!(
    rustsearch_desktop,
    [
        FocusSearch,
        OpenIndex,
        IndexFolder,
        NextResult,
        PreviousResult,
        OpenSelected,
        RevealSelected,
        CopyPath,
        ClearSearch,
        ToggleTheme,
        FocusResults
    ]
);

#[derive(Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub dark: bool,
    pub last_index: Option<PathBuf>,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            dark: true,
            last_index: None,
        }
    }
}
pub fn data_directory() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("RustSearch")
}
impl Settings {
    pub fn load() -> Self {
        std::fs::read(data_directory().join("settings.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }
    fn save(&self) {
        let root = data_directory();
        if std::fs::create_dir_all(&root).is_ok()
            && let Ok(bytes) = serde_json::to_vec_pretty(self)
        {
            let _ = std::fs::write(root.join("settings.json"), bytes);
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Search,
    Largest,
    Overview,
}

pub struct Workspace {
    engine: Engine,
    search: Entity<InputState>,
    extension: Entity<InputState>,
    focus: FocusHandle,
    list_focus: FocusHandle,
    scroll: UniformListScrollHandle,
    settings: Settings,
    source: Option<Arc<SourceInfo>>,
    source_path: Option<PathBuf>,
    rows: Vec<Row>,
    total: usize,
    selected: usize,
    generation: u64,
    text: String,
    ext: String,
    kind: Kind,
    mode: Mode,
    min_size: Option<u64>,
    limit: usize,
    page: Page,
    inspector: bool,
    busy: bool,
    busy_label: String,
    progress: Option<u64>,
    pending_root: Option<String>,
    query_ms: f64,
    error: Option<String>,
    notice: Option<String>,
    _subscriptions: Vec<Subscription>,
}

impl Workspace {
    pub fn new(
        settings: Settings,
        initial: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let search =
            cx.new(|cx| InputState::new(window, cx).placeholder("Search file and folder names…"));
        let extension = cx.new(|cx| InputState::new(window, cx).placeholder("Extension"));
        let subscriptions = vec![
            cx.subscribe_in(
                &search,
                window,
                |this, input, event, window, cx| match event {
                    InputEvent::Change => {
                        this.text = input.read(cx).value().to_string();
                        this.limit = 500;
                        this.submit(cx);
                    }
                    InputEvent::PressEnter { secondary } => {
                        this.open_selected(*secondary, cx);
                    }
                    _ => {
                        let _ = window;
                    }
                },
            ),
            cx.subscribe_in(&extension, window, |this, input, event, _, cx| {
                if matches!(event, InputEvent::Change) {
                    this.ext = input.read(cx).value().to_string();
                    this.limit = 500;
                    this.submit(cx);
                }
            }),
        ];
        let engine = Engine::new();
        let events = engine.events.clone();
        cx.spawn(async move |this, cx| {
            while let Ok(event) = events.recv().await {
                if this.update(cx, |this, cx| this.receive(event, cx)).is_err() {
                    break;
                }
            }
        })
        .detach();
        let mut view = Self {
            engine,
            search,
            extension,
            focus: cx.focus_handle(),
            list_focus: cx.focus_handle(),
            scroll: UniformListScrollHandle::new(),
            settings,
            source: None,
            source_path: None,
            rows: Vec::new(),
            total: 0,
            selected: 0,
            generation: 0,
            text: String::new(),
            ext: String::new(),
            kind: Kind::Any,
            mode: Mode::Contains,
            min_size: None,
            limit: 500,
            page: Page::Search,
            inspector: true,
            busy: false,
            busy_label: String::new(),
            progress: None,
            pending_root: None,
            query_ms: 0.,
            error: None,
            notice: None,
            _subscriptions: subscriptions,
        };
        view.search.update(cx, |s, cx| s.focus(window, cx));
        if let Some(path) = initial {
            view.open_source(path, cx);
        }
        view
    }
    fn palette(&self) -> Palette {
        Palette::new(self.settings.dark)
    }
    fn spec(&self) -> SearchSpec {
        SearchSpec {
            text: self.text.clone(),
            kind: self.kind,
            mode: self.mode,
            extension: (!self.ext.trim().is_empty()).then(|| self.ext.trim().to_string()),
            min_size: self.min_size,
            largest: self.page == Page::Largest,
            limit: self.limit,
        }
    }
    fn open_source(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.generation += 1;
        self.busy = true;
        self.error = None;
        self.notice = None;
        self.busy_label = "Opening index".into();
        self.rows.clear();
        self.source = None;
        self.pending_root = Some(path.display().to_string());
        self.progress = None;
        self.source_path = Some(path.clone());
        self.engine.submit(
            Request::Open {
                path,
                generation: self.generation,
                search: self.spec(),
            },
            self.generation,
        );
        cx.notify();
    }
    fn submit(&mut self, cx: &mut Context<Self>) {
        if self.source.is_none() {
            cx.notify();
            return;
        }
        self.generation += 1;
        self.busy = true;
        self.error = None;
        self.notice = None;
        self.busy_label = "Searching".into();
        self.engine.submit(
            Request::Search {
                generation: self.generation,
                search: self.spec(),
            },
            self.generation,
        );
        cx.notify();
    }
    fn receive(&mut self, event: Event, cx: &mut Context<Self>) {
        match event {
            Event::Progress {
                generation,
                records,
            } if generation == self.generation => {
                self.progress = Some(records);
            }
            Event::Results {
                generation,
                source,
                total,
                rows,
                query_ms,
            } if generation == self.generation => {
                if self.settings.last_index.as_ref() != Some(&source.path) {
                    self.settings.last_index = Some(source.path.clone());
                    self.settings.save();
                }
                self.progress = None;
                self.pending_root = None;
                self.source_path = Some(source.path.clone());
                self.source = Some(source);
                self.rows = rows;
                self.total = total;
                self.query_ms = query_ms;
                self.selected = 0;
                self.busy = false;
                self.scroll.scroll_to_item_strict(0, ScrollStrategy::Top);
            }
            Event::Error {
                generation,
                message,
            } if generation == self.generation => {
                self.error = Some(message);
                self.busy = false;
            }
            _ => return,
        }
        cx.notify();
    }
    fn prompt(&mut self, folder: bool, cx: &mut Context<Self>) {
        let chosen = cx.prompt_for_paths(PathPromptOptions {
            files: !folder,
            directories: folder,
            multiple: false,
            prompt: Some(
                if folder {
                    "Choose a folder to index"
                } else {
                    "Open a RustSearch .rsidx index"
                }
                .into(),
            ),
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = chosen.await
                && let Some(path) = paths.into_iter().next()
            {
                let _ = this.update(cx, |this, cx| {
                    if folder {
                        this.generation += 1;
                        this.busy = true;
                        this.error = None;
                        this.notice = None;
                        this.rows.clear();
                        this.source = None;
                        this.busy_label = "Indexing folder".into();
                        this.pending_root = Some(path.display().to_string());
                        this.progress = Some(0);
                        let stamp = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_millis();
                        let output = data_directory().join(format!(
                            "folder-{stamp}-{}-{}.rsidx",
                            std::process::id(),
                            this.generation
                        ));
                        this.source_path = Some(output.clone());
                        this.engine.submit(
                            Request::ScanFolder {
                                root: path,
                                output,
                                generation: this.generation,
                                search: this.spec(),
                            },
                            this.generation,
                        );
                        cx.notify();
                    } else {
                        this.open_source(path, cx);
                    }
                });
            }
        })
        .detach();
    }
    fn open_selected(&mut self, reveal: bool, cx: &mut Context<Self>) {
        if self.busy || self.page == Page::Overview {
            return;
        }
        if let Some(path) = self.rows.get(self.selected).and_then(|r| r.path.as_ref()) {
            if path.contains(&0) {
                self.error = Some("This path contains an invalid null character.".into());
                cx.notify();
                return;
            }
            let path = path_buf(path);
            if !path.exists() {
                self.error = Some("This item is no longer at its indexed location. Reload a refreshed index to update the snapshot.".into());
                cx.notify();
                return;
            }
            if reveal {
                cx.reveal_path(&path);
            } else {
                cx.open_with_system(&path);
            }
        }
    }
    fn copy_selected(&mut self, cx: &mut Context<Self>) {
        if self.busy || self.page == Page::Overview {
            return;
        }
        if let Some(path) = self.rows.get(self.selected).and_then(|r| r.path.as_ref()) {
            match String::from_utf16(path) {
                Ok(path) => { cx.write_to_clipboard(ClipboardItem::new_string(display_path(&path))); self.notice = Some("Path copied".into()); }
                Err(_) => self.error = Some("This path cannot be represented as ordinary text. Use Show in Explorer to preserve its original characters.".into()),
            }
            cx.notify();
        }
    }
    fn move_selection(&mut self, offset: isize, window: &mut Window, cx: &mut Context<Self>) {
        if self.rows.is_empty() || self.busy || self.page == Page::Overview {
            return;
        }
        self.selected = self
            .selected
            .saturating_add_signed(offset)
            .min(self.rows.len() - 1);
        self.scroll
            .scroll_to_item(self.selected, ScrollStrategy::Top);
        self.list_focus.focus(window);
        cx.notify();
    }
    fn cancel(&mut self, cx: &mut Context<Self>) {
        self.generation += 1;
        self.engine.submit(Request::Cancel, self.generation);
        self.source = None;
        self.source_path = None;
        self.rows.clear();
        self.total = 0;
        self.busy = false;
        self.progress = None;
        self.pending_root = None;
        self.notice = Some("Index operation cancelled".into());
        cx.notify();
    }
    fn reset_filters(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.kind = Kind::Any;
        self.mode = Mode::Contains;
        self.min_size = None;
        self.ext.clear();
        self.page = Page::Search;
        self.extension
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.submit(cx);
    }
    fn clear_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.search.update(cx, |input, cx| {
            input.set_value("", window, cx);
            input.focus(window, cx);
        });
    }
    fn toggle_theme(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.settings.dark = !self.settings.dark;
        self.settings.save();
        theme::configure(self.settings.dark, Some(window), cx);
        cx.notify();
    }
    fn nav(
        &self,
        id: &'static str,
        icon: IconName,
        title: &'static str,
        selected: bool,
    ) -> gpui::Stateful<gpui::Div> {
        let p = self.palette();
        div()
            .id(id)
            .flex()
            .items_center()
            .gap_3()
            .h(px(36.))
            .px_3()
            .rounded(px(6.))
            .cursor_pointer()
            .text_size(px(13.))
            .text_color(if selected { p.accent } else { p.muted })
            .bg(if selected { p.selected } else { p.sidebar })
            .hover(move |s| s.bg(p.raised).text_color(p.text))
            .child(Icon::new(icon).size(px(16.)))
            .child(title)
    }
    fn sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = self.palette();
        let source = self.source.clone();
        div()
            .w(px(196.))
            .flex_shrink_0()
            .h_full()
            .flex()
            .flex_col()
            .bg(p.sidebar)
            .border_r_1()
            .border_color(p.border)
            .child(
                div()
                    .px_5()
                    .pt_6()
                    .pb_5()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(
                        div()
                            .size(px(30.))
                            .rounded(px(8.))
                            .bg(p.accent)
                            .text_color(p.accent_text)
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(Icon::new(IconName::Search).size(px(18.))),
                    )
                    .child(
                        div()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_size(px(17.))
                            .child("RustSearch"),
                    ),
            )
            .child(
                div()
                    .px_3()
                    .text_size(px(10.))
                    .text_color(p.muted)
                    .pb_2()
                    .pl_5()
                    .child("WORKSPACE"),
            )
            .child(
                div()
                    .px_2()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        self.nav(
                            "nav-search",
                            IconName::Search,
                            "All items",
                            self.page == Page::Search && matches!(self.kind, Kind::Any),
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            if this.busy && this.source.is_none() {
                                return;
                            }
                            this.page = Page::Search;
                            this.kind = Kind::Any;
                            this.submit(cx);
                        })),
                    )
                    .child(
                        self.nav(
                            "nav-files",
                            IconName::File,
                            "Files",
                            self.page == Page::Search && matches!(self.kind, Kind::File),
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            if this.busy && this.source.is_none() {
                                return;
                            }
                            this.page = Page::Search;
                            this.kind = Kind::File;
                            this.submit(cx);
                        })),
                    )
                    .child(
                        self.nav(
                            "nav-folders",
                            IconName::Folder,
                            "Folders",
                            self.page == Page::Search && matches!(self.kind, Kind::Folder),
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            if this.busy && this.source.is_none() {
                                return;
                            }
                            this.page = Page::Search;
                            this.kind = Kind::Folder;
                            this.submit(cx);
                        })),
                    )
                    .child(
                        self.nav(
                            "nav-largest",
                            IconName::SortDescending,
                            "Largest files",
                            self.page == Page::Largest,
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            if this.busy && this.source.is_none() {
                                return;
                            }
                            this.page = Page::Largest;
                            this.kind = Kind::File;
                            this.submit(cx);
                        })),
                    ),
            )
            .child(div().mx_4().my_5().border_t_1().border_color(p.border))
            .child(
                div()
                    .px_5()
                    .flex()
                    .items_center()
                    .justify_between()
                    .text_size(px(10.))
                    .text_color(p.muted)
                    .child("SOURCE")
                    .child(
                        Button::new("add-source")
                            .ghost()
                            .xsmall()
                            .icon(IconName::Plus)
                            .tooltip("Index a folder · Ctrl+Shift+O")
                            .on_click(cx.listener(|this, _, _, cx| this.prompt(true, cx))),
                    ),
            )
            .child(div().px_3().pt_3().child(if let Some(source) = &source {
                div()
                    .p_3()
                    .rounded(px(7.))
                    .border_1()
                    .border_color(p.border)
                    .bg(p.surface)
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .items_center()
                            .text_color(p.text)
                            .child(Icon::new(IconName::FolderClosed).size(px(15.)))
                            .child(
                                div()
                                    .flex_1()
                                    .truncate()
                                    .text_size(px(12.))
                                    .child(source_title(&source.root)),
                            ),
                    )
                    .child(
                        div()
                            .text_size(px(11.))
                            .text_color(p.muted)
                            .child(format!("{} items", number(source.records))),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .text_size(px(10.))
                            .text_color(p.accent)
                            .child(div().size(px(5.)).rounded_full().bg(p.accent))
                            .child("Snapshot loaded"),
                    )
                    .into_any_element()
            } else {
                div()
                    .px_2()
                    .text_size(px(12.))
                    .text_color(p.muted)
                    .child("No index selected")
                    .into_any_element()
            }))
            .child(
                div().px_4().pt_3().child(
                    Button::new("open-source")
                        .ghost()
                        .small()
                        .w_full()
                        .icon(IconName::FolderOpen)
                        .label("Open index")
                        .on_click(cx.listener(|this, _, _, cx| this.prompt(false, cx))),
                ),
            )
            .child(div().flex_1())
            .child(
                div().px_2().pb_2().child(
                    self.nav(
                        "nav-overview",
                        IconName::LayoutDashboard,
                        "Index overview",
                        self.page == Page::Overview,
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        if this.busy && this.source.is_none() {
                            return;
                        }
                        this.page = Page::Overview;
                        cx.notify();
                    })),
                ),
            )
            .child(
                div().p_3().border_t_1().border_color(p.border).child(
                    Button::new("theme")
                        .ghost()
                        .small()
                        .w_full()
                        .icon(if self.settings.dark {
                            IconName::Sun
                        } else {
                            IconName::Moon
                        })
                        .label(if self.settings.dark {
                            "Light appearance"
                        } else {
                            "Dark appearance"
                        })
                        .on_click(cx.listener(|this, _, window, cx| this.toggle_theme(window, cx))),
                ),
            )
    }
    fn toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = self.palette();
        let mode_view = cx.entity().downgrade();
        let size_view = mode_view.clone();
        let mode_tag = match self.mode {
            Mode::Contains => 0,
            Mode::Prefix => 1,
            Mode::Exact => 2,
        };
        let current_size = self.min_size;
        div()
            .flex()
            .flex_col()
            .gap_4()
            .p_5()
            .pb_4()
            .border_b_1()
            .border_color(p.border)
            .bg(p.canvas)
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_3()
                            .child(
                                div()
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_size(px(17.))
                                    .child(match self.page {
                                        Page::Search => "File search",
                                        Page::Largest => "Largest files",
                                        Page::Overview => "Index overview",
                                    }),
                            )
                            .when(self.source.is_some(), |d| {
                                d.child(
                                    div()
                                        .px_2()
                                        .py_1()
                                        .rounded(px(4.))
                                        .bg(p.raised)
                                        .text_size(px(10.))
                                        .text_color(p.muted)
                                        .child("LOCAL SNAPSHOT"),
                                )
                            }),
                    )
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .child(
                                Button::new("reload")
                                    .ghost()
                                    .small()
                                    .icon(IconName::Redo)
                                    .tooltip("Reload the latest committed snapshot and deltas")
                                    .disabled(self.source_path.is_none() || self.busy)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        if let Some(path) = this.source_path.clone() {
                                            this.open_source(path, cx);
                                        }
                                    })),
                            )
                            .child(
                                Button::new("inspector")
                                    .ghost()
                                    .small()
                                    .icon(IconName::PanelRight)
                                    .tooltip("Toggle file details")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.inspector = !this.inspector;
                                        cx.notify();
                                    })),
                            ),
                    ),
            )
            .when(self.page != Page::Overview, |d| {
                d.child(
                    Input::new(&self.search)
                        .large()
                        .prefix(
                            Icon::new(IconName::Search)
                                .size(px(19.))
                                .text_color(p.accent),
                        )
                        .cleanable(true)
                        .disabled(self.source.is_none()),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            Button::new("mode")
                                .small()
                                .ghost()
                                .label(match self.mode {
                                    Mode::Contains => "Contains ▾",
                                    Mode::Prefix => "Starts with ▾",
                                    Mode::Exact => "Exact name ▾",
                                })
                                .disabled(self.source.is_none())
                                .dropdown_menu(move |mut menu, _, _| {
                                    for (label, mode, tag) in [
                                        ("Contains", Mode::Contains, 0),
                                        ("Starts with", Mode::Prefix, 1),
                                        ("Exact name", Mode::Exact, 2),
                                    ] {
                                        let view = mode_view.clone();
                                        menu = menu.item(
                                            PopupMenuItem::new(label)
                                                .checked(mode_tag == tag)
                                                .on_click(move |_, _, cx| {
                                                    let _ = view.update(cx, |this, cx| {
                                                        this.mode = mode;
                                                        this.submit(cx);
                                                    });
                                                }),
                                        );
                                    }
                                    menu
                                }),
                        )
                        .child(div().h(px(18.)).border_r_1().border_color(p.border))
                        .child(
                            div().w(px(145.)).child(
                                Input::new(&self.extension)
                                    .small()
                                    .disabled(self.source.is_none()),
                            ),
                        )
                        .child(
                            Button::new("minimum-size")
                                .ghost()
                                .small()
                                .label(match self.min_size {
                                    None => "Any size ▾",
                                    Some(1_048_576) => "≥ 1 MiB ▾",
                                    Some(104_857_600) => "≥ 100 MiB ▾",
                                    _ => "≥ 1 GiB ▾",
                                })
                                .disabled(self.source.is_none())
                                .dropdown_menu(move |mut menu, _, _| {
                                    for (label, value) in [
                                        ("Any size", None),
                                        ("At least 1 MiB", Some(1_048_576)),
                                        ("At least 100 MiB", Some(104_857_600)),
                                        ("At least 1 GiB", Some(1_073_741_824)),
                                    ] {
                                        let view = size_view.clone();
                                        menu = menu.item(
                                            PopupMenuItem::new(label)
                                                .checked(current_size == value)
                                                .on_click(move |_, _, cx| {
                                                    let _ = view.update(cx, |this, cx| {
                                                        this.min_size = value;
                                                        this.submit(cx);
                                                    });
                                                }),
                                        );
                                    }
                                    menu
                                }),
                        )
                        .child(div().flex_1())
                        .child(
                            Button::new("reset-filters")
                                .ghost()
                                .xsmall()
                                .label("Reset")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.reset_filters(window, cx)
                                })),
                        ),
                )
            })
    }
    fn welcome(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = self.palette();
        div().flex_1().flex().flex_col().justify_center().items_center().gap_5().p_8()
            .child(div().size(px(66.)).rounded(px(18.)).bg(p.selected).text_color(p.accent).flex().items_center().justify_center().child(Icon::new(IconName::Search).size(px(31.))))
            .child(div().text_size(px(25.)).font_weight(gpui::FontWeight::SEMIBOLD).child("Your files, within reach."))
            .child(div().text_color(p.muted).text_size(px(14.)).max_w(px(400.)).text_center().child("Index a folder, or open an existing disk index. Search names instantly without reading file contents."))
            .child(div().flex().gap_3()
                .child(Button::new("welcome-index").primary().label("Index a folder").icon(IconName::Plus).on_click(cx.listener(|this, _, _, cx| this.prompt(true, cx))))
                .child(Button::new("welcome-open").outline().label("Open index").icon(IconName::FolderOpen).on_click(cx.listener(|this, _, _, cx| this.prompt(false, cx)))))
            .child(div().text_size(px(11.)).text_color(p.muted).child("Local metadata  ·  No account  ·  No cloud"))
    }
    fn results(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = self.palette();
        if self.source.is_none() {
            if self.busy {
                return div()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap_4()
                    .child(Spinner::new().color(p.accent))
                    .child(self.busy_label.clone())
                    .when_some(self.pending_root.clone(), |d, root| {
                        d.child(
                            div()
                                .max_w(px(560.))
                                .truncate()
                                .text_size(px(12.))
                                .text_color(p.muted)
                                .child(root),
                        )
                    })
                    .when_some(self.progress, |d, count| {
                        d.child(
                            div()
                                .text_size(px(12.))
                                .text_color(p.accent)
                                .child(format!("{} items found", number(count as usize))),
                        )
                    })
                    .child(
                        Button::new("cancel-index")
                            .outline()
                            .small()
                            .label("Cancel")
                            .on_click(cx.listener(|this, _, _, cx| this.cancel(cx))),
                    )
                    .child(
                        div().text_size(px(12.)).text_color(p.muted).child(
                            "File contents stay untouched. The interface remains responsive.",
                        ),
                    )
                    .into_any_element();
            }
            return self.welcome(cx).into_any_element();
        }
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(
                div()
                    .h(px(37.))
                    .px_5()
                    .flex()
                    .items_center()
                    .gap_3()
                    .bg(p.surface)
                    .border_b_1()
                    .border_color(p.border)
                    .text_size(px(11.))
                    .text_color(p.muted)
                    .child(div().flex_1().min_w_0().child("NAME"))
                    .child(div().flex_1().min_w_0().child("LOCATION"))
                    .child(div().w(px(64.)).child("TYPE"))
                    .child(div().w(px(78.)).text_right().child("SIZE")),
            )
            .child(if self.rows.is_empty() {
                div()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap_3()
                    .child(
                        Icon::new(IconName::Search)
                            .size(px(28.))
                            .text_color(p.muted),
                    )
                    .child(div().text_size(px(17.)).child("No matching items"))
                    .child(
                        div()
                            .text_size(px(12.))
                            .text_color(p.muted)
                            .child("Try a shorter name or reset the filters."),
                    )
                    .child(
                        Button::new("empty-reset")
                            .ghost()
                            .small()
                            .label("Reset filters")
                            .on_click(
                                cx.listener(|this, _, window, cx| this.reset_filters(window, cx)),
                            ),
                    )
                    .into_any_element()
            } else {
                uniform_list(
                    "file-results",
                    self.rows.len(),
                    cx.processor(|this, range: std::ops::Range<usize>, _, cx| {
                        range
                            .map(|ix| this.result_row(ix, cx).into_any_element())
                            .collect()
                    }),
                )
                .track_scroll(self.scroll.clone())
                .w_full()
                .h_full()
                .flex_1()
                .into_any_element()
            })
            .when(
                self.rows.len() < self.total && self.rows.len() < 10_000,
                |d| {
                    d.child(
                        div()
                            .py_2()
                            .flex()
                            .justify_center()
                            .border_t_1()
                            .border_color(p.border)
                            .child(
                                Button::new("load-more")
                                    .ghost()
                                    .small()
                                    .label("Show 500 more results")
                                    .disabled(self.busy)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.limit = (this.limit + 500).min(10_000);
                                        this.submit(cx);
                                    })),
                            ),
                    )
                },
            )
            .into_any_element()
    }
    fn result_row(&self, ix: usize, cx: &mut Context<Self>) -> impl IntoElement {
        let p = self.palette();
        let row = &self.rows[ix];
        let selected = ix == self.selected;
        let kind = if row.directory {
            "Folder".into()
        } else {
            row.name
                .rsplit_once('.')
                .map(|(_, ext)| ext.to_uppercase())
                .filter(|e| e.len() <= 8)
                .unwrap_or_else(|| "File".into())
        };
        div()
            .id(ix)
            .w_full()
            .h(px(41.))
            .px_5()
            .flex()
            .items_center()
            .gap_3()
            .border_b_1()
            .border_color(p.border.opacity(0.35))
            .bg(if selected { p.selected } else { p.canvas })
            .hover(move |s| s.bg(if selected { p.selected } else { p.surface }))
            .cursor_pointer()
            .on_click(
                cx.listener(move |this, event: &gpui::ClickEvent, window, cx| {
                    this.selected = ix;
                    this.list_focus.focus(window);
                    if event.click_count() >= 2 {
                        this.open_selected(false, cx);
                    }
                    cx.notify();
                }),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap_3()
                    .overflow_hidden()
                    .child(
                        Icon::new(if row.directory {
                            IconName::Folder
                        } else {
                            IconName::File
                        })
                        .size(px(17.))
                        .text_color(if row.directory {
                            p.warning
                        } else {
                            p.accent
                        }),
                    )
                    .child(
                        div()
                            .truncate()
                            .text_size(px(13.))
                            .text_color(p.text)
                            .child(row.name.clone()),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(px(12.))
                    .text_color(p.muted)
                    .child(display_path(&row.location)),
            )
            .child(
                div()
                    .w(px(64.))
                    .truncate()
                    .text_size(px(10.))
                    .text_color(p.muted)
                    .child(kind),
            )
            .child(
                div()
                    .w(px(78.))
                    .text_right()
                    .text_size(px(12.))
                    .text_color(if row.size.is_some() { p.text } else { p.muted })
                    .child(row.size.map(bytes).unwrap_or_else(|| "—".into())),
            )
    }
    fn details(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = self.palette();
        let row = self.rows.get(self.selected);
        div().w(px(264.)).flex_shrink_0().h_full().border_l_1().border_color(p.border).bg(p.surface).flex().flex_col()
            .child(div().px_5().h(px(47.)).flex().items_center().justify_between().border_b_1().border_color(p.border)
                .child(div().text_size(px(12.)).font_weight(gpui::FontWeight::SEMIBOLD).child("ITEM DETAILS"))
                .child(Button::new("close-details").ghost().xsmall().icon(IconName::Close).on_click(cx.listener(|this, _, _, cx| { this.inspector = false; cx.notify(); }))))
            .child(if let Some(row) = row {
                div().flex_1().min_h_0().id("details-scroll").overflow_y_scroll().p_5().flex().flex_col().gap_4()
                    .child(div().size(px(58.)).rounded(px(12.)).bg(p.raised).flex().items_center().justify_center().text_color(if row.directory { p.warning } else { p.accent }).child(Icon::new(if row.directory { IconName::Folder } else { IconName::File }).size(px(29.))))
                    .child(div().text_size(px(16.)).font_weight(gpui::FontWeight::SEMIBOLD).child(row.name.clone()))
                    .child(div().text_size(px(11.)).text_color(p.muted).child(if row.directory { "Folder" } else { "File" }))
                    .child(Button::new("open-file").primary().w_full().label(if row.directory { "Open folder" } else { "Open file" }).icon(IconName::ExternalLink).disabled(self.busy || row.path.is_none()).on_click(cx.listener(|this, _, _, cx| this.open_selected(false, cx))))
                    .child(Button::new("reveal-file").outline().w_full().label("Show in Explorer").icon(IconName::FolderOpen).disabled(self.busy || row.path.is_none()).on_click(cx.listener(|this, _, _, cx| this.open_selected(true, cx))))
                    .child(Button::new("copy-path").ghost().w_full().label("Copy path").icon(IconName::Copy).disabled(self.busy || row.path.is_none()).on_click(cx.listener(|this, _, _, cx| this.copy_selected(cx))))
                    .child(div().h(px(1.)).bg(p.border))
                    .child(detail("LOCATION", display_path(&row.location), p))
                    .child(detail("LOGICAL SIZE", row.size.map(bytes).unwrap_or_else(|| "Not collected in this index".into()), p))
                    .child(detail("FILE REFERENCE", format!("{:016X}", row.id), p))
                    .when(row.lossy_display, |d| d.child(div().text_size(px(11.)).text_color(p.warning).child("Display uses replacement characters. The original path is preserved for file actions.")))
                    .when(row.path_error.is_some(), |d| d.child(div().text_size(px(11.)).text_color(p.warning).child(row.path_error.clone().unwrap_or_default())))
                    .into_any_element()
            } else { div().p_5().text_color(p.muted).text_size(px(12.)).child("Select an item to inspect its location and metadata.").into_any_element() })
    }
    fn overview(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = self.palette();
        let Some(source) = &self.source else {
            return self.welcome(cx).into_any_element();
        };
        let coverage = if source.files == 0 {
            0.
        } else {
            source.sized_files as f64 / source.files as f64
        };
        div().flex_1().id("overview-scroll").overflow_y_scroll().p_6().flex().flex_col().gap_6()
            .child(div().text_size(px(23.)).font_weight(gpui::FontWeight::SEMIBOLD).child("One index. A clear view."))
            .child(div().text_color(p.muted).text_size(px(13.)).child("Search uses a point-in-time metadata snapshot. Reload after an external refresh to pick up committed changes."))
            .child(div().flex().gap_4()
                .child(stat("TOTAL ITEMS", number(source.records), p))
                .child(stat("FILES", number(source.files), p))
                .child(stat("FOLDERS", number(source.folders), p)))
            .child(div().p_5().rounded(px(9.)).border_1().border_color(p.border).bg(p.surface).flex().flex_col().gap_3()
                .child(div().flex().justify_between().text_size(px(14.)).child("File-size coverage").child(format!("{:.2}%", coverage * 100.)))
                .child(div().h(px(5.)).rounded_full().bg(p.raised).child(div().h_full().w(relative(coverage as f32)).rounded_full().bg(p.accent)))
                .child(div().text_size(px(12.)).text_color(p.muted).child(format!("{} of {} files have a known logical size. Unknown sizes are excluded from largest-file and size-filtered results.", number(source.sized_files), number(source.files)))))
            .child(div().flex().gap_4()
                .child(stat("INDEX MEMORY", bytes(source.memory_bytes as u64), p))
                .child(stat("DELTA GENERATION", source.sequence.to_string(), p))
                .child(stat("PENDING REFRESH", number(source.pending), p)))
            .child(detail("SOURCE", display_path(&source.root), p))
            .child(detail("BACKEND", if source.backend == "ntfs-usn" { "Native NTFS metadata" } else { "Folder traversal" }.into(), p))
            .child(detail("PREPARATION", format!("Source {:.0} ms · search tables {:.0} ms · cache {}", source.load_ms, source.prepare_ms, source.cache_state), p))
            .child(div().text_size(px(12.)).text_color(p.muted).child("One canonical name per file reference. Logical sizes describe the default data stream; they are not allocated or reclaimable disk space."))
            .into_any_element()
    }
    fn footer(&self) -> impl IntoElement {
        let p = self.palette();
        div()
            .h(px(33.))
            .px_4()
            .flex_shrink_0()
            .flex()
            .items_center()
            .justify_between()
            .border_t_1()
            .border_color(p.border)
            .bg(p.sidebar)
            .text_size(px(11.))
            .text_color(p.muted)
            .child(div().flex().items_center().gap_3().child(if self.busy {
                format!("{}…", self.busy_label)
            } else if let Some(notice) = &self.notice {
                notice.clone()
            } else if self.source.is_some() {
                format!(
                    "{} matches · {} shown · {:.2} ms",
                    number(self.total),
                    number(self.rows.len()),
                    self.query_ms
                )
            } else {
                "Ready when you are".into()
            }))
            .child(if let Some(source) = &self.source {
                format!(
                    "Index {} · snapshot #{}",
                    bytes(source.memory_bytes as u64),
                    source.sequence
                )
            } else {
                "Ctrl+O open index · Ctrl+Shift+O index folder".into()
            })
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = self.palette();
        let inspector = self.inspector
            && self.source.is_some()
            && self.page != Page::Overview
            && window.viewport_size().width >= px(1080.);
        div()
            .key_context("RustSearch")
            .track_focus(&self.focus)
            .size_full()
            .flex()
            .flex_col()
            .bg(p.canvas)
            .text_color(p.text)
            .font_family("Segoe UI")
            .text_size(px(14.))
            .on_action(cx.listener(|this, _: &FocusSearch, window, cx| {
                this.search.update(cx, |input, cx| input.focus(window, cx))
            }))
            .on_action(cx.listener(|this, _: &OpenIndex, _, cx| this.prompt(false, cx)))
            .on_action(cx.listener(|this, _: &IndexFolder, _, cx| this.prompt(true, cx)))
            .on_action(
                cx.listener(|this, _: &NextResult, window, cx| this.move_selection(1, window, cx)),
            )
            .on_action(cx.listener(|this, _: &PreviousResult, window, cx| {
                this.move_selection(-1, window, cx)
            }))
            .on_action(cx.listener(|this, _: &OpenSelected, _, cx| this.open_selected(false, cx)))
            .on_action(cx.listener(|this, _: &RevealSelected, _, cx| this.open_selected(true, cx)))
            .on_action(cx.listener(|this, _: &CopyPath, _, cx| this.copy_selected(cx)))
            .on_action(
                cx.listener(|this, _: &ClearSearch, window, cx| this.clear_search(window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &ToggleTheme, window, cx| this.toggle_theme(window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &FocusResults, window, _| this.list_focus.focus(window)),
            )
            .child(
                TitleBar::new().bg(p.sidebar).child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .text_size(px(11.))
                        .text_color(p.muted)
                        .child("RustSearch")
                        .child("·")
                        .child("Local file search"),
                ),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .child(self.sidebar(cx))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(self.toolbar(cx))
                            .when_some(self.error.clone(), |d, error| {
                                d.child(
                                    div()
                                        .mx_5()
                                        .mt_3()
                                        .p_3()
                                        .rounded(px(6.))
                                        .bg(p.selected)
                                        .text_color(p.warning)
                                        .text_size(px(12.))
                                        .child(error)
                                        .child(
                                            Button::new("dismiss-error")
                                                .ghost()
                                                .xsmall()
                                                .label("Dismiss")
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    this.error = None;
                                                    cx.notify();
                                                })),
                                        ),
                                )
                            })
                            .child(
                                div()
                                    .flex_1()
                                    .min_h_0()
                                    .flex()
                                    .flex_col()
                                    .key_context(if self.page == Page::Overview {
                                        "RustSearchOverview"
                                    } else {
                                        "RustSearchResults"
                                    })
                                    .track_focus(&self.list_focus)
                                    .child(if self.page == Page::Overview {
                                        self.overview(cx).into_any_element()
                                    } else {
                                        self.results(cx).into_any_element()
                                    }),
                            ),
                    )
                    .when(inspector, |d| d.child(self.details(cx))),
            )
            .child(self.footer())
    }
}
fn source_title(root: &str) -> String {
    let clean = display_path(root);
    std::path::Path::new(&clean)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or(clean)
}
fn detail(label: &'static str, value: String, p: Palette) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .gap_2()
        .child(div().text_size(px(10.)).text_color(p.muted).child(label))
        .child(div().text_size(px(12.)).text_color(p.text).child(value))
}
fn stat(label: &'static str, value: String, p: Palette) -> impl IntoElement {
    div()
        .flex_1()
        .p_5()
        .rounded(px(9.))
        .border_1()
        .border_color(p.border)
        .bg(p.surface)
        .flex()
        .flex_col()
        .gap_3()
        .child(div().text_size(px(10.)).text_color(p.muted).child(label))
        .child(
            div()
                .text_size(px(24.))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .child(value),
        )
}
pub fn number(value: usize) -> String {
    let digits = value.to_string();
    let mut result = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            result.push(',');
        }
        result.push(c);
    }
    result
}
pub fn bytes(value: u64) -> String {
    if value < 1024 {
        return format!("{value} B");
    }
    let units = ["KiB", "MiB", "GiB", "TiB", "PiB", "EiB"];
    let mut size = value as f64 / 1024.;
    let mut i = 0;
    while size >= 1024. && i < units.len() - 1 {
        size /= 1024.;
        i += 1;
    }
    format!("{size:.1} {}", units[i])
}
fn path_buf(units: &[u16]) -> PathBuf {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStringExt;
        PathBuf::from(std::ffi::OsString::from_wide(units))
    }
    #[cfg(not(windows))]
    {
        PathBuf::from(String::from_utf16_lossy(units))
    }
}
