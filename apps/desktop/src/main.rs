mod backend;
mod components;
mod design;
mod icons;
mod notifications;
mod state;
mod view;

use anyhow::{Context, Result};
use clap::Parser;
use gpui::{App, Bounds, TitlebarOptions, WindowBounds, WindowOptions, prelude::*, px, size};
use gpui_elements::editable_text::actions::{DEFAULT_INPUT_CONTEXT, default_bindings};
use std::{path::PathBuf, sync::Arc, time::Duration};

#[derive(Parser)]
#[command(name = "tachikoma-gui", about = "A local decision system")]
struct Args {
    #[arg(long, value_enum)]
    appearance: Option<design::Appearance>,
    #[arg(long)]
    socket: Option<PathBuf>,
    #[arg(long)]
    credential: Option<PathBuf>,
    /// Run notifications without opening a window. Closing a window also keeps notifications active.
    #[arg(long, conflicts_with = "demo")]
    background: bool,
    /// Open a specific review in the existing desktop instance.
    #[arg(long, conflicts_with = "demo")]
    request: Option<String>,
    /// Open an offline, read-only example; never contacts the approval service.
    #[arg(long)]
    demo: bool,
    #[arg(long, hide = true, requires = "demo")]
    smoke_test: bool,
}
fn main() -> Result<()> {
    let args = Args::parse();
    let state = dirs::state_dir()
        .context("state directory unavailable")?
        .join("tachikoma");
    let runtime_directory = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| state.join("run"));
    let backend = backend::Backend {
        socket: args
            .socket
            .unwrap_or_else(|| runtime_directory.join("tachikoma/tachikoma.sock")),
        credential: args
            .credential
            .unwrap_or_else(|| state.join("approval/desktop.token")),
        demo: args.demo,
    };
    let runtime = Arc::new(
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()?,
    );
    let preferences_path = state.join("desktop/appearance.json");
    let (sender, mut incoming) = tokio::sync::mpsc::channel(32);
    let _instance = if args.demo {
        None
    } else {
        match runtime.block_on(notifications::instance(
            sender.clone(),
            args.background,
            args.request.as_deref().unwrap_or(""),
        ))? {
            Some(bus) => Some(bus),
            None => return Ok(()),
        }
    };
    if !args.demo {
        runtime.spawn(notifications::run(backend.clone(), sender.clone()));
    }
    gpui_platform::application()
        .with_assets(icons::assets()?)
        .run(move |cx: &mut App| {
            cx.bind_keys(default_bindings().as_keybindings(Some(DEFAULT_INPUT_CONTEXT)));
            view::bind_keys(cx);
            if args.smoke_test {
                cx.observe_keystrokes(|event, _, _| eprintln!("Smoke keystroke: {event:?}"))
                    .detach();
            }
            if !args.demo {
                cx.set_quit_mode(gpui::QuitMode::Explicit);
            }
            if args.demo {
                cx.on_window_closed(|cx, _| {
                    if cx.windows().is_empty() {
                        cx.quit();
                    }
                })
                .detach();
            }
            let mut window = None;
            if !args.background {
                window = Some(open_window(
                    &backend,
                    &runtime,
                    args.appearance,
                    &preferences_path,
                    cx,
                ));
            }
            if let (Some(handle), Some(id)) = (window, args.request) {
                let _ = handle.update(cx, |view, window, cx| view.open_review(id, window, cx));
            }
            if args.smoke_test
                && let Some(handle) = window
            {
                let _ = handle.update(cx, |_, window, _| {
                    window.on_next_frame(|_, cx| {
                        eprintln!("Decision UI first frame rendered");
                        cx.spawn(async move |cx| {
                            cx.background_executor()
                                .timer(Duration::from_secs(12))
                                .await;
                            cx.update(|cx| {
                                eprintln!("Decision UI native smoke test complete");
                                cx.quit();
                            });
                        })
                        .detach();
                    });
                });
            }
            cx.spawn(async move |cx| {
                while let Some(open) = incoming.recv().await {
                    cx.update(|cx| {
                        let valid =
                            window.is_some_and(|handle| handle.update(cx, |_, _, _| ()).is_ok());
                        if !valid {
                            window = Some(open_window(
                                &backend,
                                &runtime,
                                args.appearance,
                                &preferences_path,
                                cx,
                            ));
                        }
                        if let Some(handle) = window {
                            let _ = handle.update(cx, |view, window, cx| {
                                if !open.request.is_empty() {
                                    view.open_review(open.request, window, cx);
                                }
                                if !window.activate_with_token(&open.token) {
                                    window.activate();
                                }
                            });
                            cx.activate(true);
                        }
                    });
                }
            })
            .detach();
        });
    Ok(())
}
fn open_window(
    backend: &backend::Backend,
    runtime: &Arc<tokio::runtime::Runtime>,
    appearance: Option<design::Appearance>,
    preferences_path: &std::path::Path,
    cx: &mut App,
) -> gpui::WindowHandle<view::Desktop> {
    let bounds = Bounds::centered(None, size(px(1320.), px(860.)), cx);
    let result = cx.open_window(
        WindowOptions::new()
            .window_bounds(Some(WindowBounds::Windowed(bounds)))
            .window_min_size(Some(size(px(1000.), px(620.))))
            .app_id(Some("dev.noel.tachikoma".to_owned()))
            .titlebar(Some(TitlebarOptions {
                title: Some("tachikoma".into()),
                ..Default::default()
            })),
        |window, cx| {
            cx.new(|cx| {
                view::Desktop::new(
                    backend.clone(),
                    Arc::clone(runtime),
                    appearance,
                    preferences_path.to_owned(),
                    window,
                    cx,
                )
            })
        },
    );
    match result {
        Ok(handle) => {
            cx.activate(true);
            handle
        }
        Err(_) => {
            eprintln!("Unable to open decision window");
            std::process::exit(1);
        }
    }
}
