#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]
mod assets;
mod theme;
mod view;
use gpui::{
    App, AppContext, Application, Bounds, KeyBinding, WindowBounds, WindowOptions, px, size,
};
use gpui_component::Root;
use view::*;

fn main() {
    let mut settings = Settings::load();
    let mut initial = settings.last_index.clone().filter(|path| path.is_file());
    let mut compact = false;
    let mut args = std::env::args_os().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--index" {
            initial = args.next().map(Into::into);
        } else if arg == "--no-restore" {
            initial = None;
        } else if arg == "--light" {
            settings.dark = false;
        } else if arg == "--compact" {
            compact = true;
        }
    }
    Application::new()
        .with_assets(assets::Assets)
        .run(move |cx: &mut App| {
            gpui_component::init(cx);
            theme::configure(settings.dark, None, cx);
            cx.bind_keys([
                KeyBinding::new("ctrl-f", FocusSearch, Some("RustSearch")),
                KeyBinding::new("ctrl-l", FocusSearch, Some("RustSearch")),
                KeyBinding::new("ctrl-o", OpenIndex, Some("RustSearch")),
                KeyBinding::new("ctrl-shift-o", IndexFolder, Some("RustSearch")),
                KeyBinding::new("ctrl-j", NextResult, Some("RustSearch")),
                KeyBinding::new("ctrl-k", PreviousResult, Some("RustSearch")),
                KeyBinding::new("f6", FocusResults, Some("RustSearch")),
                KeyBinding::new("escape", ClearSearch, Some("RustSearch")),
                KeyBinding::new("ctrl-shift-c", CopyPath, Some("RustSearch")),
                KeyBinding::new("ctrl-shift-t", ToggleTheme, Some("RustSearch")),
                KeyBinding::new("down", NextResult, Some("RustSearchResults")),
                KeyBinding::new("up", PreviousResult, Some("RustSearchResults")),
                KeyBinding::new("enter", OpenSelected, Some("RustSearchResults")),
                KeyBinding::new("ctrl-enter", RevealSelected, Some("RustSearchResults")),
            ]);
            let dimensions = if compact {
                size(px(960.), px(600.))
            } else {
                size(px(1260.), px(790.))
            };
            let bounds = Bounds::centered(None, dimensions, cx);
            cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    titlebar: Some(gpui::TitlebarOptions {
                        title: Some("RustSearch".into()),
                        appears_transparent: true,
                        ..Default::default()
                    }),
                    window_min_size: Some(size(px(960.), px(600.))),
                    ..Default::default()
                },
                |window, cx| {
                    let workspace = cx.new(|cx| Workspace::new(settings, initial, window, cx));
                    cx.new(|cx| Root::new(workspace, window, cx))
                },
            )
            .expect("open RustSearch window");
            cx.on_window_closed(|cx| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();
            cx.activate(true);
        });
}
